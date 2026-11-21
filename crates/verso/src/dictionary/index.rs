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
/// hundreds spread over the file (docs/dictionary.md#implementation-decisions).
const MIN_BUCKETS: usize = 4096;
const MAX_BUCKETS: usize = 1 << 18;
const SIGNATURE: u64 = 64;
/// Fixed part of a record: next, offset, size and the two lengths.
const RECORD: usize = 24;
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
struct Record {
    next: u64,
    offset: u64,
    size: u32,
    key: String,
    word: String,
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
fn hash(text: &str, buckets: usize) -> usize {
    (text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3)) % buckets as u64)
        as usize
}
fn read_word(r: &mut impl BufRead) -> Result<Option<String>> {
    let mut data = Vec::with_capacity(256);
    // Limit before allocating: malformed sources cannot grow this buffer.
    r.take(MAX_WORD_BYTES as u64 + 1).read_until(0, &mut data)?;
    if data.is_empty() {
        return Ok(None);
    }
    ensure!(data.last() == Some(&0) || data.len() <= MAX_WORD_BYTES, "Headword too long (limit 4096 bytes)");
    ensure!(data.last() == Some(&0), "Unterminated headword");
    data.pop();
    ensure!(!data.is_empty(), "Empty index word");
    Ok(Some(String::from_utf8(data)?))
}
/// The folded lookup key for a source headword, within the cache limit.
/// Folding can lengthen text, so a source word that fits may not.
fn folded_key(word: &str) -> Result<String> {
    let key = fold(word);
    ensure!(key.len() <= MAX_WORD_BYTES, "Folded headword too long (limit 4096 bytes)");
    Ok(key)
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
        let file = &mut self.output;
        let heads = &mut self.heads;
        let bucket = hash(key, heads.len());
        let at = self.cursor;
        file.write_all(&heads[bucket].to_le_bytes())?;
        file.write_all(&offset.to_le_bytes())?;
        file.write_all(&size.to_le_bytes())?;
        file.write_all(&(key.len() as u16).to_le_bytes())?;
        file.write_all(&(display.len() as u16).to_le_bytes())?;
        file.write_all(key.as_bytes())?;
        file.write_all(display.as_bytes())?;
        heads[bucket] = at;
        self.cursor += 24 + key.len() as u64 + display.len() as u64;
        Ok(at)
    }
}
/// The record at `at`, in two reads: its fixed part, then both words. On the
/// Kindle every read of user storage goes through FUSE, so this matters
/// more than it looks.
fn record(file: &File, len: u64, header: u64, at: u64) -> Result<Record> {
    ensure!(at >= header && at < len, "Invalid cached index pointer");
    let mut fixed = [0; RECORD];
    file.read_exact_at(&mut fixed, at)?;
    let next = u64::from_le_bytes(fixed[..8].try_into()?);
    let offset = u64::from_le_bytes(fixed[8..16].try_into()?);
    let size = u32::from_le_bytes(fixed[16..20].try_into()?);
    let klen = u16::from_le_bytes(fixed[20..22].try_into()?) as usize;
    let wlen = u16::from_le_bytes(fixed[22..].try_into()?) as usize;
    ensure!(klen <= MAX_WORD_BYTES && wlen <= MAX_WORD_BYTES, "Invalid cached index word");
    let mut words = vec![0; klen + wlen];
    file.read_exact_at(&mut words, at + RECORD as u64)?;
    let word = String::from_utf8(words.split_off(klen))?;
    Ok(Record { next, offset, size, key: String::from_utf8(words)?, word })
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
        let result = (|| {
            let mut out = BufWriter::with_capacity(65536, File::create(&temp)?);
            out.write_all(fingerprint.as_bytes())?;
            out.write_all(&vec![0; (header - SIGNATURE) as usize])?;
            let mut builder = Builder { output: out, heads: vec![0u64; buckets], cursor: header };
            let mut refs = BufWriter::new(File::create(&refs_path)?);
            let source = File::open(&idx)?;
            let input: Box<dyn Read> = if compressed { Box::new(GzDecoder::new(source)) } else { Box::new(source) };
            let mut input = BufReader::new(input);
            let mut count = 0u32;
            let mut bytes = 0u64;
            loop {
                if count.is_multiple_of(128) {
                    ensure!(current(), "Lookup cancelled");
                }
                // Only source-record problems get the record context; cache
                // write errors (a full disk, say) are reported as they are.
                let entry = (|| -> Result<Option<(String, String, u64, u32)>> {
                    let Some(word) = read_word(&mut input)? else { return Ok(None) };
                    ensure!(count < info.count, "Index has more words than wordcount");
                    let offset = if info.wide {
                        let mut b = [0; 8];
                        input.read_exact(&mut b).context("Truncated offset/size record")?;
                        u64::from_be_bytes(b)
                    } else {
                        read_u32_be(&mut input).context("Truncated offset/size record")? as u64
                    };
                    let size = read_u32_be(&mut input).context("Truncated offset/size record")?;
                    ensure!(
                        offset.checked_add(size as u64).is_some_and(|end| end <= data_len),
                        "Index points outside definitions"
                    );
                    let key = folded_key(&word)?;
                    Ok(Some((word, key, offset, size)))
                })()
                .with_context(|| format!("{}: .idx record {count} at byte offset {bytes}", idx.display()))?;
                let Some((word, key, offset, size)) = entry else { break };
                let at = builder.append(&key, &word, offset, size)?;
                refs.write_all(&at.to_le_bytes())?;
                bytes += word.len() as u64 + 1 + if info.wide { 12 } else { 8 };
                count += 1;
            }
            ensure!(
                count == info.count && bytes == info.index_size,
                "{}: .idx record {count} at byte offset {bytes}: Truncated index or count/size differs from .ifo",
                idx.display()
            );
            builder.output.flush()?;
            refs.flush()?;
            drop(refs);
            if syn.exists() {
                let mut refs = File::open(&refs_path)?;
                let originals = File::open(&temp)?;
                let originals_len = originals.metadata()?.len();
                let mut input = BufReader::new(File::open(&syn)?);
                let mut count = 0u32;
                let mut bytes = 0u64;
                loop {
                    if count.is_multiple_of(128) {
                        ensure!(current(), "Lookup cancelled");
                    }
                    let alias = (|| -> Result<Option<(String, String, u32)>> {
                        let Some(word) = read_word(&mut input)? else { return Ok(None) };
                        ensure!(count < info.syn_count, "Synonym count differs from .ifo");
                        let target = read_u32_be(&mut input).context("Truncated synonym target record")?;
                        ensure!(target < info.count, "Invalid synonym target");
                        let key = folded_key(&word)?;
                        Ok(Some((word, key, target)))
                    })()
                    .with_context(|| format!("{}: .syn record {count} at byte offset {bytes}", syn.display()))?;
                    let Some((word, key, target)) = alias else { break };
                    // The target's record comes from the partly built cache.
                    refs.seek(SeekFrom::Start(target as u64 * 8))?;
                    let at = read_u64(&mut refs)?;
                    let original = record(&originals, originals_len, header, at)?;
                    builder.append(&key, &original.word, original.offset, original.size)?;
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
            for head in builder.heads {
                out.write_all(&head.to_le_bytes())?;
            }
            out.flush()?;
            out.get_ref().sync_all()?;
            fs::rename(&temp, &dest)?;
            let file = File::open(&dest)?;
            let len = file.metadata()?.len();
            Ok(Self { file, len, buckets })
        })();
        let _ = fs::remove_file(&temp);
        let _ = fs::remove_file(&refs_path);
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
            let entry = record(&self.file, self.len, header, at)?;
            ensure!(entry.next == 0 || entry.next < at, "Cyclic cached index");
            at = entry.next;
            if entry.key == key {
                // Prefer the earliest source record for a repeated span. A
                // later alias must not move a homograph ahead of its siblings.
                if let Some(i) = entries.iter().position(|(_, o, s)| *o == entry.offset && *s == entry.size) {
                    entries.remove(i);
                }
                entries.push((entry.word, entry.offset, entry.size));
                ensure!(entries.len() <= 128, "Too many entries for this word");
            }
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
}
