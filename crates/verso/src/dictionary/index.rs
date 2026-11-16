//! Disk hash index of Unicode-folded keys. Constant-size build buffers, no
//! resident word list, and a single open index. The source ordinal map lets
//! synonyms point to the original entry even when offsets are 64 bit.
use super::{Info, basename, companion, sibling};
use crate::library::search::fold;
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
const MAX_WORD_BYTES: usize = 4096;
const BUCKETS: usize = 4096;
const SIGNATURE: u64 = 64;
const HEADER: u64 = SIGNATURE + BUCKETS as u64 * 8;
static BUILD_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
type Entry = (String, u64, u32);
struct Record {
    next: u64,
    offset: u64,
    size: u32,
    key: String,
    word: String,
}
pub struct Index(File);
fn hash(text: &str) -> usize {
    text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3)) as usize % BUCKETS
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
        let bucket = hash(key);
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
fn record(file: &mut File, at: u64) -> Result<Record> {
    ensure!(at >= HEADER && at < file.metadata()?.len(), "Invalid cached index pointer");
    file.seek(SeekFrom::Start(at))?;
    let next = read_u64(file)?;
    let offset = read_u64(file)?;
    let mut sizes = [0; 8];
    file.read_exact(&mut sizes)?;
    let size = u32::from_le_bytes(sizes[..4].try_into()?);
    let klen = u16::from_le_bytes(sizes[4..6].try_into()?) as usize;
    let wlen = u16::from_le_bytes(sizes[6..].try_into()?) as usize;
    ensure!(klen <= MAX_WORD_BYTES && wlen <= MAX_WORD_BYTES, "Invalid cached index word");
    let mut key = vec![0; klen];
    let mut word = vec![0; wlen];
    file.read_exact(&mut key)?;
    file.read_exact(&mut word)?;
    Ok(Record { next, offset, size, key: String::from_utf8(key)?, word: String::from_utf8(word)? })
}
fn cache_location(path: &Path, cache: &Path) -> Result<(PathBuf, String)> {
    let base = basename(path).context("Not a StarDict .ifo or .ifo.gz file")?;
    let (idx, _) = companion(&base, "idx", "idx.gz")?;
    let (dict, _) = companion(&base, "dict", "dict.dz")?;
    let syn = sibling(&base, "syn");
    let mut signature = Sha256::new();
    signature.update(b"verso-stardict-index-v1");
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
/// The cache file when it exists and carries this fingerprint; the one rule
/// for whether a cache is valid. A cache that can't be opened is rebuilt.
fn cached(dest: &Path, fingerprint: &str) -> Result<Option<File>> {
    let Ok(mut file) = File::open(dest) else { return Ok(None) };
    if file.metadata()?.len() < HEADER {
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
        data_len: u64,
        current: &impl Fn() -> bool,
        preparing: &impl Fn(),
    ) -> Result<Self> {
        fs::create_dir_all(cache)?;
        let (dest, fingerprint) = cache_location(path, cache)?;
        if let Some(file) = cached(&dest, &fingerprint)? {
            return Ok(Self(file));
        }
        let base = basename(path).context("Not a StarDict .ifo or .ifo.gz file")?;
        let (idx, compressed) = companion(&base, "idx", "idx.gz")?;
        let syn = sibling(&base, "syn");
        preparing();
        let build_id = BUILD_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let suffix = format!("{}-{build_id}", std::process::id());
        let temp = dest.with_extension(format!("{suffix}.tmp"));
        let refs_path = dest.with_extension(format!("{suffix}.refs"));
        let result = (|| {
            let mut out = BufWriter::with_capacity(65536, File::create(&temp)?);
            out.write_all(fingerprint.as_bytes())?;
            out.write_all(&vec![0; HEADER as usize - SIGNATURE as usize])?;
            let mut builder = Builder { output: out, heads: vec![0u64; BUCKETS], cursor: HEADER };
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
                let mut originals = File::open(&temp)?;
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
                    let original = record(&mut originals, at)?;
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
            Ok(Self(File::open(&dest)?))
        })();
        let _ = fs::remove_file(&temp);
        let _ = fs::remove_file(&refs_path);
        result
    }
    pub fn find(&mut self, query: &str) -> Result<Vec<Entry>> {
        ensure!(query.len() <= MAX_WORD_BYTES, "Query is too long");
        let key = fold(query.trim());
        ensure!(key.len() <= MAX_WORD_BYTES, "Folded query is too long (limit 4096 bytes)");
        self.0.seek(SeekFrom::Start(SIGNATURE + hash(&key) as u64 * 8))?;
        let mut at = read_u64(&mut self.0)?;
        let mut entries = Vec::new();
        while at != 0 {
            let entry = record(&mut self.0, at)?;
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
