//! Disk hash index of Unicode-folded keys. Bounded build buffers, no
//! resident word list, and a single open index. The source ordinal map lets
//! synonyms point to the original entry even when offsets are 64 bit.
use super::{Info, basename, companion, sibling};
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
};
const MAX_WORD_BYTES: usize = 4096;
/// Bucket counts are powers of two between these: about two words a
/// bucket, so a lookup reads two or three records instead of a chain of
/// hundreds spread over the file.
const MIN_BUCKETS: usize = 4096;
const MAX_BUCKETS: usize = 1 << 18;
const SIGNATURE: u64 = 64;
/// Fixed part of a record: next, offset, size and the two lengths.
const RECORD: usize = 24;
/// Bytes fetched in a single `pread` before falling back to a second read for
/// unusually long headwords (24-byte header + up to 232 bytes of key + word).
const INLINE_READ: usize = 256;
/// Upper bounds for resolving `.syn` entries from a transient in-memory slab
/// instead of writing `.refs` and doing random reads on `.tmp`.
const MAX_SLAB_WORDS: usize = 500_000;
const MAX_SLAB_WORD_BYTES: usize = 16 * 1024 * 1024;
/// Part of the cache fingerprint: bump it whenever `fold` (in verso-wordforms)
/// changes, so every cached index rebuilds once with the new keys.
const KEY_VERSION: &[u8] = b"key-v2";
/// A lookup that walks more records than this in one bucket is refused: a
/// sound index holds a handful, so only crafted collisions get this long.
const MAX_CHAIN: usize = 20_000;
/// How long an index build's temp file must sit untouched before another
/// process's leftovers are swept: a build keeps writing it.
const STALE_BUILD: std::time::Duration = std::time::Duration::from_secs(600);
static BUILD_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
type Entry = (String, u64, u32);
/// The bucket count for a dictionary's headwords and synonyms. Derived from
/// the `.ifo` counts, which the cache fingerprint covers, so building and
/// opening always agree.
fn bucket_count(info: &Info) -> usize {
    let entries = info.count as usize + info.syn_count as usize;
    (entries / 2).next_power_of_two().clamp(MIN_BUCKETS, MAX_BUCKETS)
}
fn header_len(buckets: usize) -> u64 {
    SIGNATURE + buckets as u64 * 8
}
struct RecordHeader {
    next: u64,
    offset: u64,
    size: u32,
}
struct Record {
    offset: u64,
    size: u32,
    word: String,
}
/// Compact in-memory table of source headwords used during `.syn` resolution so
/// building an index with synonyms does zero `.refs` file I/O and zero random
/// reads against the partly built `.tmp` file.
struct SynSlab {
    offsets: Vec<u64>,
    sizes: Vec<u32>,
    word_starts: Vec<u32>,
    word_lens: Vec<u16>,
    words: String,
    max_word_bytes: usize,
}
impl SynSlab {
    fn new(max_word_bytes: usize) -> Self {
        Self {
            offsets: Vec::new(),
            sizes: Vec::new(),
            word_starts: Vec::new(),
            word_lens: Vec::new(),
            words: String::new(),
            max_word_bytes,
        }
    }
    fn push(&mut self, offset: u64, size: u32, word: &str) -> Result<()> {
        ensure!(
            self.words.len().saturating_add(word.len()) <= self.max_word_bytes,
            "Truncated index or count/size differs from .ifo"
        );
        let start = u32::try_from(self.words.len()).context("Synonym slab overflow")?;
        let len = u16::try_from(word.len()).context("Headword too long")?;
        self.offsets.push(offset);
        self.sizes.push(size);
        self.word_starts.push(start);
        self.word_lens.push(len);
        self.words.push_str(word);
        Ok(())
    }
    fn get(&self, target: usize) -> Option<(&str, u64, u32)> {
        let offset = *self.offsets.get(target)?;
        let size = *self.sizes.get(target)?;
        let start = *self.word_starts.get(target)? as usize;
        let len = *self.word_lens.get(target)? as usize;
        Some((&self.words[start..start + len], offset, size))
    }
}
enum SynRefs {
    None,
    Slab(SynSlab),
    Disk(BufWriter<File>),
}
enum SynLookup {
    Slab(SynSlab),
    Disk { refs: File, originals: File, originals_len: u64, header: u64 },
}
/// An open index cache. The file never changes once built (a rebuild writes
/// a new file and renames it), so its length is read once.
pub struct Index {
    file: File,
    len: u64,
    buckets: usize,
}
/// The dictionary key for a headword or query, shared with the word-form
/// packs: case folded and normalized, and blind to Russian ё/е and stress
/// marks, so `ещё` finds `еще` and `молоко́` finds `молоко`.
pub use verso_wordforms::fold;
use verso_wordforms::fold_into;
fn hash(text: &str, buckets: usize) -> usize {
    (text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3)) % buckets as u64)
        as usize
}
fn read_word<'a>(r: &mut impl BufRead, buf: &'a mut Vec<u8>) -> Result<Option<&'a str>> {
    buf.clear();
    // Limit before allocating: malformed sources cannot grow this buffer.
    r.take(MAX_WORD_BYTES as u64 + 1).read_until(0, buf)?;
    if buf.is_empty() {
        return Ok(None);
    }
    ensure!(buf.last() == Some(&0) || buf.len() <= MAX_WORD_BYTES, "Headword too long (limit 4096 bytes)");
    ensure!(buf.last() == Some(&0), "Unterminated headword");
    buf.pop();
    ensure!(!buf.is_empty(), "Empty index word");
    Ok(Some(std::str::from_utf8(buf)?))
}
/// The folded lookup key for a source headword, within the cache limit.
/// Folding can lengthen text, so a source word that fits may not.
fn folded_key(word: &str, key: &mut String) -> Result<()> {
    fold_into(word, key);
    ensure!(key.len() <= MAX_WORD_BYTES, "Folded headword too long (limit 4096 bytes)");
    Ok(())
}
/// Reads the next headword and its definition span, folding its key into
/// `key` (reused across records, like `word_buf`).
fn read_idx_record<'a>(
    input: &mut impl BufRead,
    word_buf: &'a mut Vec<u8>,
    key: &mut String,
    info: &Info,
    data_len: u64,
    count: u32,
) -> Result<Option<(&'a str, u64, u32)>> {
    let Some(word) = read_word(input, word_buf)? else { return Ok(None) };
    ensure!(count < info.count, "Index has more words than wordcount");
    let offset = if info.wide {
        let mut b = [0; 8];
        input.read_exact(&mut b).context("Truncated offset/size record")?;
        u64::from_be_bytes(b)
    } else {
        read_u32_be(input).context("Truncated offset/size record")? as u64
    };
    let size = read_u32_be(input).context("Truncated offset/size record")?;
    ensure!(offset.checked_add(size as u64).is_some_and(|end| end <= data_len), "Index points outside definitions");
    folded_key(word, key)?;
    Ok(Some((word, offset, size)))
}
fn read_syn_record<'a>(
    input: &mut impl BufRead,
    word_buf: &'a mut Vec<u8>,
    key: &mut String,
    info: &Info,
    count: u32,
) -> Result<Option<(&'a str, u32)>> {
    let Some(word) = read_word(input, word_buf)? else { return Ok(None) };
    ensure!(count < info.syn_count, "Synonym count differs from .ifo");
    let target = read_u32_be(input).context("Truncated synonym target record")?;
    ensure!(target < info.count, "Invalid synonym target");
    folded_key(word, key)?;
    Ok(Some((word, target)))
}
/// `false` when a lookup query exceeds the index limits, before or after
/// folding. Such a query cannot match any headword.
pub fn query_fits(query: &str) -> bool {
    query.len() <= MAX_WORD_BYTES && fold(query.trim()).len() <= MAX_WORD_BYTES
}
fn read_u64_at(file: &File, at: u64) -> Result<u64> {
    let mut b = [0; 8];
    file.read_exact_at(&mut b, at)?;
    Ok(u64::from_le_bytes(b))
}
fn read_u64(r: &mut impl Read) -> Result<u64> {
    let mut b = [0; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn read_u32_be(r: &mut impl Read) -> Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b))
}
struct Builder {
    output: BufWriter<File>,
    heads: Vec<u64>,
    cursor: u64,
}
impl Builder {
    /// Appends a record for an already folded `key` (see `folded_key`).
    fn append(&mut self, key: &str, display: &str, offset: u64, size: u32) -> Result<u64> {
        let bucket = hash(key, self.heads.len());
        let at = self.cursor;
        let mut fixed = [0u8; RECORD];
        fixed[..8].copy_from_slice(&self.heads[bucket].to_le_bytes());
        fixed[8..16].copy_from_slice(&offset.to_le_bytes());
        fixed[16..20].copy_from_slice(&size.to_le_bytes());
        fixed[20..22].copy_from_slice(&(key.len() as u16).to_le_bytes());
        fixed[22..24].copy_from_slice(&(display.len() as u16).to_le_bytes());
        self.output.write_all(&fixed)?;
        self.output.write_all(key.as_bytes())?;
        self.output.write_all(display.as_bytes())?;
        self.heads[bucket] = at;
        self.cursor += RECORD as u64 + key.len() as u64 + display.len() as u64;
        Ok(at)
    }
}
/// Reads the record at `at` in one `pread` of up to `INLINE_READ` bytes when
/// the key and display word fit (almost every dictionary entry), falling back
/// to a second `pread` only for unusually long headwords. On the Kindle every
/// read of user storage goes through FUSE, so cutting `pread`s in half matters.
fn read_record<T>(
    file: &File,
    len: u64,
    header: u64,
    at: u64,
    f: impl FnOnce(RecordHeader, &str, &str) -> Result<T>,
) -> Result<T> {
    ensure!(at >= header && at < len, "Invalid cached index pointer");
    let want = (len - at).min(INLINE_READ as u64) as usize;
    ensure!(want >= RECORD, "Truncated cached index record");
    let mut inline = [0u8; INLINE_READ];
    file.read_exact_at(&mut inline[..want], at)?;
    let next = u64::from_le_bytes(inline[..8].try_into()?);
    let offset = u64::from_le_bytes(inline[8..16].try_into()?);
    let size = u32::from_le_bytes(inline[16..20].try_into()?);
    let klen = u16::from_le_bytes(inline[20..22].try_into()?) as usize;
    let wlen = u16::from_le_bytes(inline[22..24].try_into()?) as usize;
    ensure!(klen <= MAX_WORD_BYTES && wlen <= MAX_WORD_BYTES, "Invalid cached index word");
    let hdr = RecordHeader { next, offset, size };
    let words_len = klen + wlen;
    let total = RECORD + words_len;
    if total <= want {
        let key = std::str::from_utf8(&inline[RECORD..RECORD + klen])?;
        let word = std::str::from_utf8(&inline[RECORD + klen..total])?;
        return f(hdr, key, word);
    }
    ensure!(at.checked_add(total as u64).is_some_and(|end| end <= len), "Truncated cached index record");
    let have = want - RECORD;
    let mut words = vec![0u8; words_len];
    words[..have].copy_from_slice(&inline[RECORD..want]);
    file.read_exact_at(&mut words[have..], at + want as u64)?;
    let key = std::str::from_utf8(&words[..klen])?;
    let word = std::str::from_utf8(&words[klen..])?;
    f(hdr, key, word)
}
fn record(file: &File, len: u64, header: u64, at: u64) -> Result<Record> {
    read_record(file, len, header, at, |hdr, _key, word| {
        Ok(Record { offset: hdr.offset, size: hdr.size, word: word.to_owned() })
    })
}
fn cache_location(path: &Path, cache: &Path) -> Result<(PathBuf, String)> {
    let base = basename(path).context("Not a StarDict .ifo or .ifo.gz file")?;
    let (idx, _) = companion(&base, "idx", "idx.gz")?;
    let (dict, _) = companion(&base, "dict", "dict.dz")?;
    let syn = sibling(&base, "syn");
    let mut signature = Sha256::new();
    signature.update(b"verso-stardict-index-v2");
    signature.update(KEY_VERSION);
    // `path` is the metadata actually read (`.ifo` or `.ifo.gz`), so
    // switching or replacing either variant changes the signature.
    for source in [path, &idx, &dict, &syn] {
        signature.update(source.as_os_str().as_encoded_bytes());
        if source.exists() {
            let meta = source.metadata()?;
            signature.update(meta.len().to_le_bytes());
            signature.update(meta.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_nanos().to_le_bytes());
        }
    }
    let fingerprint = format!("{:x}", signature.finalize());
    // Named after the plain `.ifo` path for either variant: existing caches
    // stay valid, and a replaced variant overwrites rather than orphans.
    let name = format!("{:x}", Sha256::digest(sibling(&base, "ifo").as_os_str().as_encoded_bytes()));
    let dest = cache.join(format!("{name}.index"));
    Ok((dest, fingerprint))
}
/// Removes the cached index file for the dictionary whose `.ifo` or `.ifo.gz`
/// is `path`. A missing cache file is ignored.
pub(crate) fn remove_cache(path: &Path, cache: &Path) {
    let Some(base) = basename(path) else { return };
    let name = format!("{:x}", Sha256::digest(sibling(&base, "ifo").as_os_str().as_encoded_bytes()));
    let dest = cache.join(format!("{name}.index"));
    if let Err(e) = fs::remove_file(&dest)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("removing dictionary cache {}: {e}", dest.display());
    }
}
/// Removes the `<name>.<pid>-<n>.tmp` and `.refs` files of builds that a
/// crash or power loss cut off. A build removes its own on return, and the
/// shared worker runs one at a time, so files with this process's id are
/// never touched; another process's are left while they are still changing.
fn sweep_stale_builds(cache: &Path) {
    let Ok(entries) = fs::read_dir(cache) else { return };
    let ours = std::process::id().to_string();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let mut parts = name.to_string_lossy().split('.').map(str::to_owned).collect::<Vec<_>>();
        let [_, run, extension] = parts.as_mut_slice() else { continue };
        let Some((pid, build)) = run.split_once('-') else { continue };
        let leftover = matches!(extension.as_str(), "tmp" | "refs")
            && !pid.is_empty()
            && !build.is_empty()
            && pid.bytes().chain(build.bytes()).all(|b| b.is_ascii_digit() || b == b'-')
            && pid != ours;
        let idle =
            entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age >= STALE_BUILD));
        if leftover && idle && fs::remove_file(entry.path()).is_ok() {
            log::info!("removed a stale dictionary index build file: {}", entry.path().display());
        }
    }
}
/// The cache file when it exists and carries this fingerprint; the one rule
/// for whether a cache is valid. A cache that can't be opened is rebuilt.
fn cached(dest: &Path, fingerprint: &str) -> Result<Option<File>> {
    let Ok(mut file) = File::open(dest) else { return Ok(None) };
    if file.metadata()?.len() < SIGNATURE {
        return Ok(None);
    }
    let mut signature = [0; SIGNATURE as usize];
    file.read_exact(&mut signature)?;
    Ok((signature == fingerprint.as_bytes()).then_some(file))
}
impl Index {
    /// Whether a valid cache exists, without building one.
    pub fn prepared(path: &Path, cache: &Path) -> bool {
        cache_location(path, cache)
            .and_then(|(dest, fingerprint)| cached(&dest, &fingerprint))
            .is_ok_and(|f| f.is_some())
    }
    pub fn open(
        path: &Path,
        cache: &Path,
        info: &Info,
        data_len: impl FnOnce() -> Result<u64>,
        current: &impl Fn() -> bool,
        preparing: &impl Fn(),
    ) -> Result<Self> {
        Self::open_with_slab_limits(
            path,
            cache,
            info,
            data_len,
            current,
            preparing,
            MAX_SLAB_WORDS,
            MAX_SLAB_WORD_BYTES,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn open_with_slab_limits(
        path: &Path,
        cache: &Path,
        info: &Info,
        data_len: impl FnOnce() -> Result<u64>,
        current: &impl Fn() -> bool,
        preparing: &impl Fn(),
        max_slab_words: usize,
        max_slab_word_bytes: usize,
    ) -> Result<Self> {
        fs::create_dir_all(cache)?;
        let (dest, fingerprint) = cache_location(path, cache)?;
        let buckets = bucket_count(info);
        let header = header_len(buckets);
        if let Some(file) = cached(&dest, &fingerprint)? {
            let len = file.metadata()?.len();
            // A cut-off file with the right signature is rebuilt.
            if len >= header {
                return Ok(Self { file, len, buckets });
            }
        }
        let base = basename(path).context("Not a StarDict .ifo or .ifo.gz file")?;
        let (idx, compressed) = companion(&base, "idx", "idx.gz")?;
        let syn = sibling(&base, "syn");
        // Only a build reads the definitions file, to bound each record.
        let data_len = data_len()?;
        preparing();
        let started = std::time::Instant::now();
        log::debug!("dictionary index build started: {}", path.display());
        sweep_stale_builds(cache);
        let build_id = BUILD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let suffix = format!("{}-{build_id}", std::process::id());
        let temp = dest.with_extension(format!("{suffix}.tmp"));
        let refs_path = dest.with_extension(format!("{suffix}.refs"));
        let has_syn = syn.exists();
        let mut created_refs = false;
        let result = (|| {
            let mut out = BufWriter::with_capacity(65536, File::create(&temp)?);
            out.write_all(fingerprint.as_bytes())?;
            let zeros = [0u8; 4096];
            let mut remaining = (header - SIGNATURE) as usize;
            while remaining > 0 {
                let n = remaining.min(zeros.len());
                out.write_all(&zeros[..n])?;
                remaining -= n;
            }
            let mut builder = Builder { output: out, heads: vec![0u64; buckets], cursor: header };
            let mut syn_refs = if !has_syn {
                SynRefs::None
            } else if info.count as usize <= max_slab_words && info.index_size <= max_slab_word_bytes as u64 {
                SynRefs::Slab(SynSlab::new(max_slab_word_bytes))
            } else {
                created_refs = true;
                SynRefs::Disk(BufWriter::with_capacity(65536, File::create(&refs_path)?))
            };
            let source = File::open(&idx)?;
            let input: Box<dyn Read> = if compressed { Box::new(GzDecoder::new(source)) } else { Box::new(source) };
            let mut input = BufReader::with_capacity(65536, input);
            let mut word_buf = Vec::with_capacity(256);
            let mut key = String::with_capacity(256);
            let mut count = 0u32;
            let mut bytes = 0u64;
            loop {
                if count.is_multiple_of(128) {
                    ensure!(current(), "Lookup cancelled");
                }
                // Only source-record problems get the record context; cache
                // write errors (a full disk, say) are reported as they are.
                let entry = read_idx_record(&mut input, &mut word_buf, &mut key, info, data_len, count)
                    .with_context(|| format!("{}: .idx record {count} at byte offset {bytes}", idx.display()))?;
                let Some((word, offset, size)) = entry else { break };
                let at = builder.append(&key, word, offset, size)?;
                match &mut syn_refs {
                    SynRefs::None => {}
                    SynRefs::Slab(slab) => slab
                        .push(offset, size, word)
                        .with_context(|| format!("{}: .idx record {count} at byte offset {bytes}", idx.display()))?,
                    SynRefs::Disk(refs) => refs.write_all(&at.to_le_bytes())?,
                }
                bytes += word.len() as u64 + 1 + if info.wide { 12 } else { 8 };
                count += 1;
            }
            ensure!(
                count == info.count && bytes == info.index_size,
                "{}: .idx record {count} at byte offset {bytes}: Truncated index or count/size differs from .ifo",
                idx.display()
            );
            let syn_lookup = match syn_refs {
                SynRefs::None => None,
                SynRefs::Slab(slab) => Some(SynLookup::Slab(slab)),
                SynRefs::Disk(mut refs) => {
                    builder.output.flush()?;
                    refs.flush()?;
                    drop(refs);
                    let refs = File::open(&refs_path)?;
                    let originals = File::open(&temp)?;
                    let originals_len = originals.metadata()?.len();
                    Some(SynLookup::Disk { refs, originals, originals_len, header })
                }
            };
            if let Some(mut lookup) = syn_lookup {
                let mut input = BufReader::with_capacity(65536, File::open(&syn)?);
                let mut count = 0u32;
                let mut bytes = 0u64;
                loop {
                    if count.is_multiple_of(128) {
                        ensure!(current(), "Lookup cancelled");
                    }
                    let alias = read_syn_record(&mut input, &mut word_buf, &mut key, info, count)
                        .with_context(|| format!("{}: .syn record {count} at byte offset {bytes}", syn.display()))?;
                    let Some((word, target)) = alias else { break };
                    match &mut lookup {
                        SynLookup::Slab(slab) => {
                            let (orig_word, orig_offset, orig_size) =
                                slab.get(target as usize).context("Invalid synonym target")?;
                            builder.append(&key, orig_word, orig_offset, orig_size)?;
                        }
                        SynLookup::Disk { refs, originals, originals_len, header } => {
                            // The target's record comes from the partly built cache.
                            refs.seek(SeekFrom::Start(target as u64 * 8))?;
                            let at = read_u64(refs)?;
                            let original = record(originals, *originals_len, *header, at)?;
                            builder.append(&key, &original.word, original.offset, original.size)?;
                        }
                    }
                    bytes += word.len() as u64 + 5;
                    count += 1;
                }
                ensure!(
                    count == info.syn_count,
                    "{}: .syn record {count} at byte offset {bytes}: Truncated index or synonym count differs from .ifo",
                    syn.display()
                );
            }
            ensure!(current(), "Lookup cancelled");
            let mut out = builder.output;
            out.seek(SeekFrom::Start(SIGNATURE))?;
            for chunk in builder.heads.chunks(512) {
                let mut buf = [0u8; 512 * 8];
                for (slot, &head) in buf.as_chunks_mut::<8>().0.iter_mut().zip(chunk) {
                    *slot = head.to_le_bytes();
                }
                out.write_all(&buf[..chunk.len() * 8])?;
            }
            out.flush()?;
            out.get_ref().sync_all()?;
            fs::rename(&temp, &dest)?;
            let file = File::open(&dest)?;
            let len = file.metadata()?.len();
            Ok(Self { file, len, buckets })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        if created_refs {
            let _ = fs::remove_file(&refs_path);
        }
        log::debug!(
            "dictionary index build finished: {:?}, success {}, {}",
            started.elapsed(),
            result.is_ok(),
            path.display()
        );
        result
    }
    pub fn find(&mut self, query: &str, current: &impl Fn() -> bool) -> Result<Vec<Entry>> {
        ensure!(query.len() <= MAX_WORD_BYTES, "Query is too long");
        let key = fold(query.trim());
        ensure!(key.len() <= MAX_WORD_BYTES, "Folded query is too long (limit 4096 bytes)");
        let header = header_len(self.buckets);
        let mut at = read_u64_at(&self.file, SIGNATURE + hash(&key, self.buckets) as u64 * 8)?;
        let mut entries = Vec::new();
        let mut steps = 0usize;
        while at != 0 {
            steps += 1;
            ensure!(steps <= MAX_CHAIN, "Too many colliding words in the cached index");
            if steps.is_multiple_of(128) {
                ensure!(current(), "Lookup cancelled");
            }
            at = read_record(&self.file, self.len, header, at, |hdr, entry_key, entry_word| {
                ensure!(hdr.next == 0 || hdr.next < at, "Cyclic cached index");
                if entry_key == key {
                    // Prefer the earliest source record for a repeated span. A
                    // later alias must not move a homograph ahead of its siblings.
                    if let Some(i) = entries.iter().position(|(_, o, s)| *o == hdr.offset && *s == hdr.size) {
                        entries.remove(i);
                    }
                    entries.push((entry_word.to_owned(), hdr.offset, hdr.size));
                    ensure!(entries.len() <= 128, "Too many entries for this word");
                }
                Ok(hdr.next)
            })?;
        }
        // Stable source order for homographs; exact spelling comes first.
        entries.reverse();
        entries.sort_by_key(|(word, _, _)| word != query.trim());
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(count: u32, syn_count: u32) -> Info {
        Info { name: String::new(), count, syn_count, index_size: 0, wide: false, types: String::new() }
    }

    fn write_fixture(dir: &Path, words: &[(&str, &str)], syns: &[(&str, u32)], wide: bool) -> (PathBuf, Info, u64) {
        let base = dir.join("dict");
        let ifo_path = base.with_extension("ifo");
        let mut idx = Vec::new();
        let mut data = Vec::new();
        for (word, def) in words {
            idx.extend_from_slice(word.as_bytes());
            idx.push(0);
            if wide {
                idx.extend_from_slice(&(data.len() as u64).to_be_bytes());
            } else {
                idx.extend_from_slice(&(data.len() as u32).to_be_bytes());
            }
            idx.extend_from_slice(&(def.len() as u32).to_be_bytes());
            data.extend_from_slice(def.as_bytes());
        }
        fs::write(&ifo_path, b"StarDict's dict ifo file\nversion=3.0.0\n").unwrap();
        fs::write(base.with_extension("idx"), &idx).unwrap();
        fs::write(base.with_extension("dict"), &data).unwrap();
        if !syns.is_empty() {
            let mut syn = Vec::new();
            for (alias, target) in syns {
                syn.extend_from_slice(alias.as_bytes());
                syn.push(0);
                syn.extend_from_slice(&target.to_be_bytes());
            }
            fs::write(base.with_extension("syn"), syn).unwrap();
        }
        let info = Info {
            name: "Test".into(),
            count: words.len() as u32,
            syn_count: syns.len() as u32,
            index_size: idx.len() as u64,
            wide,
            types: "m".into(),
        };
        (ifo_path, info, data.len() as u64)
    }

    #[test]
    fn sweeping_removes_only_other_processes_idle_build_files() {
        let dir = tempfile::tempdir().unwrap();
        let old = std::time::SystemTime::now() - STALE_BUILD * 2;
        let ours = std::process::id();
        let make = |name: String, age: Option<std::time::SystemTime>| {
            let path = dir.path().join(name);
            let file = File::create(&path).unwrap();
            if let Some(time) = age {
                file.set_modified(time).unwrap();
            }
            path
        };
        let stale_tmp = make("abc.1-0.tmp".into(), Some(old));
        let stale_refs = make("abc.1-0.refs".into(), Some(old));
        let running = make("abc.1-1.tmp".into(), None);
        let own = make(format!("abc.{ours}-0.tmp"), Some(old));
        let cache = make("abc.index".into(), Some(old));
        let other = make("abc.x-0.tmp".into(), Some(old));
        sweep_stale_builds(dir.path());
        assert!(!stale_tmp.exists() && !stale_refs.exists());
        assert!(running.exists() && own.exists() && cache.exists() && other.exists());
    }

    #[test]
    fn bucket_counts_keep_about_two_words_a_bucket_within_bounds() {
        assert_eq!(bucket_count(&info(0, 0)), MIN_BUCKETS);
        assert_eq!(bucket_count(&info(8_000, 0)), MIN_BUCKETS);
        assert_eq!(bucket_count(&info(100_000, 20_000)), 65_536);
        assert_eq!(bucket_count(&info(600_000, 0)), MAX_BUCKETS);
        assert_eq!(bucket_count(&info(u32::MAX, u32::MAX)), MAX_BUCKETS);
        assert_eq!(header_len(MIN_BUCKETS), 64 + 4096 * 8, "the old fixed table size");
    }

    #[test]
    fn syn_slab_and_disk_fallback_produce_identical_index_files() {
        let dir = tempfile::tempdir().unwrap();
        let words = [
            ("Apple", "A fruit."),
            ("Apple", "A tree."),
            ("café", "Coffee."),
            ("Ёж", "Hedgehog."),
            ("alpha", "First."),
        ];
        let syns = [("fruit", 0), ("tree", 1), ("coffee", 2), ("ежик", 3)];
        let (ifo, info, data_len) = write_fixture(dir.path(), &words, &syns, false);

        let slab_cache = dir.path().join("cache-slab");
        let disk_cache = dir.path().join("cache-disk");
        let mut slab_idx = Index::open_with_slab_limits(
            &ifo,
            &slab_cache,
            &info,
            || Ok(data_len),
            &|| true,
            &|| {},
            MAX_SLAB_WORDS,
            MAX_SLAB_WORD_BYTES,
        )
        .unwrap();
        let mut disk_idx =
            Index::open_with_slab_limits(&ifo, &disk_cache, &info, || Ok(data_len), &|| true, &|| {}, 0, 0).unwrap();

        let (slab_file, _) = cache_location(&ifo, &slab_cache).unwrap();
        let (disk_file, _) = cache_location(&ifo, &disk_cache).unwrap();
        assert_eq!(fs::read(&slab_file).unwrap(), fs::read(&disk_file).unwrap());
        for query in ["apple", "Apple", "fruit", "tree", "coffee", "ежик", "ёж", "missing"] {
            assert_eq!(slab_idx.find(query, &|| true).unwrap(), disk_idx.find(query, &|| true).unwrap(), "{query}");
        }
    }

    #[test]
    fn inline_record_read_handles_boundary_lengths_eof_and_truncation() {
        let dir = tempfile::tempdir().unwrap();
        // Record sizes right around INLINE_READ (256): header (24) + key + word.
        // ASCII lowercase has key.len() == word.len():
        //   115*2 + 24 = 254 (< 256)
        //   116*2 + 24 = 256 (== 256, exact inline fit)
        //   117*2 + 24 = 258 (> 256, triggers follow-up read)
        //   500*2 + 24 = 1024 (well past inline)
        let w254 = "a".repeat(115);
        let w256 = "b".repeat(116);
        let w258 = "c".repeat(117);
        let w1024 = "d".repeat(500);
        let words = [
            (w254.as_str(), "def-254"),
            (w256.as_str(), "def-256"),
            (w258.as_str(), "def-258"),
            (w1024.as_str(), "def-1024"),
            ("z", "last-short-at-eof"),
        ];
        let (ifo, info, data_len) = write_fixture(dir.path(), &words, &[], false);
        let cache = dir.path().join("cache");
        let mut index = Index::open(&ifo, &cache, &info, || Ok(data_len), &|| true, &|| {}).unwrap();
        for (word, _) in &words {
            let found = index.find(word, &|| true).unwrap();
            assert_eq!(found.len(), 1, "word len {}", word.len());
            assert_eq!(found[0].0, *word);
        }

        // Truncated cached record (both inside the 24-byte header and inside the words) fails cleanly.
        let (cache_file, _) = cache_location(&ifo, &cache).unwrap();
        let header = header_len(index.buckets);
        let raw = fs::read(&cache_file).unwrap();
        // Truncate inside the very first record's header.
        let trunc_hdr = dir.path().join("trunc-hdr.index");
        fs::write(&trunc_hdr, &raw[..header as usize + 10]).unwrap();
        let f = File::open(&trunc_hdr).unwrap();
        let len = f.metadata().unwrap().len();
        assert!(record(&f, len, header, header).is_err());

        // Truncate inside the first record's key/word payload.
        let trunc_body = dir.path().join("trunc-body.index");
        fs::write(&trunc_body, &raw[..header as usize + RECORD + 5]).unwrap();
        let f = File::open(&trunc_body).unwrap();
        let len = f.metadata().unwrap().len();
        assert!(record(&f, len, header, header).is_err());

        // Cyclic next pointer fails cleanly.
        let mut cyclic = raw.clone();
        cyclic[header as usize..header as usize + 8].copy_from_slice(&header.to_le_bytes());
        let cyc_path = dir.path().join("cyclic.index");
        fs::write(&cyc_path, &cyclic).unwrap();
        let cyc_file = File::open(&cyc_path).unwrap();
        let cyc_len = cyc_file.metadata().unwrap().len();
        let mut cyc_index = Index { file: cyc_file, len: cyc_len, buckets: index.buckets };
        let err = cyc_index.find(&w254, &|| true).unwrap_err();
        assert!(err.to_string().contains("Cyclic"), "{err}");
    }
}
