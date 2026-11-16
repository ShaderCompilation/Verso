use super::*;
use flate2::{Compress, Compression, FlushCompress, read::GzDecoder, write::GzEncoder};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "verso-dictionary-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("dictionaries/test.ifo")
    }
    fn install(&self, wide: bool, compressed: bool) -> Result<()> {
        self.install_words(
            wide,
            compressed,
            &[
                ("Apple", "A fruit."),
                ("Apple", "A tree."),
                ("apple", "a lowercase entry"),
                ("café", "<p>Coffee &amp; cake</p><p>Another paragraph.</p>"),
                ("Слово", "Русское определение."),
            ],
        )
    }
    fn install_words(&self, wide: bool, compressed: bool, words: &[(&str, &str)]) -> Result<()> {
        fs::create_dir_all(self.0.join("dictionaries"))?;
        let mut idx = Vec::new();
        let mut data = Vec::new();
        for (word, meaning) in words {
            idx.extend_from_slice(word.as_bytes());
            idx.push(0);
            if wide {
                idx.extend_from_slice(&(data.len() as u64).to_be_bytes());
            } else {
                idx.extend_from_slice(&(data.len() as u32).to_be_bytes());
            }
            idx.extend_from_slice(&(meaning.len() as u32).to_be_bytes());
            data.extend_from_slice(meaning.as_bytes());
        }
        fs::write(
            self.path(),
            format!(
                "StarDict's dict ifo file\nversion=3.0.0\nbookname=Test dictionary\nwordcount={}\nsynwordcount=1\nidxfilesize={}\nidxoffsetbits={}\nsametypesequence=h\n",
                words.len(),
                idx.len(),
                if wide { 64 } else { 32 }
            ),
        )?;
        fs::write(self.path().with_extension("syn"), b"fruit\0\0\0\0\0")?;
        if compressed {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&idx)?;
            fs::write(self.path().with_extension("idx.gz"), encoder.finish()?)?;
            fs::write(self.path().with_extension("dict.dz"), dictzip(&data, 32)?)?;
        } else {
            fs::write(self.path().with_extension("idx"), idx)?;
            fs::write(self.path().with_extension("dict"), data)?;
        }
        Ok(())
    }
    fn backend(&self) -> Result<Backend> {
        Backend::open(&self.path(), &self.0.join("cache"), &|| true)
    }
}
impl Fixture {
    /// Writes `test.ifo.gz` from `test.ifo`, removing the plain file unless kept.
    fn gzip_ifo(&self, keep_plain: bool) -> Result<PathBuf> {
        let gz = self.path().with_extension("ifo.gz");
        fs::write(&gz, gzip(&fs::read(self.path())?)?)?;
        if !keep_plain {
            fs::remove_file(self.path())?;
        }
        Ok(gz)
    }
    fn cache_files(&self) -> Result<Vec<(PathBuf, Vec<u8>)>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(self.0.join("cache"))? {
            let path = entry?.path();
            let mut signature = vec![0; 64];
            fs::File::open(&path)?.read_exact(&mut signature)?;
            files.push((path, signature));
        }
        Ok(files)
    }
}
fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    Ok(encoder.finish()?)
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn dictzip(data: &[u8], chunk: usize) -> Result<Vec<u8>> {
    let mut compressor = Compress::new(Compression::default(), false);
    let mut blocks = Vec::new();
    for part in data.chunks(chunk) {
        let mut buffer = vec![0; chunk * 2 + 100];
        let before = compressor.total_out();
        let input = compressor.total_in();
        compressor.compress(part, &mut buffer, FlushCompress::Full)?;
        assert_eq!(compressor.total_in() - input, part.len() as u64);
        buffer.truncate((compressor.total_out() - before) as usize);
        blocks.push(buffer);
    }
    let mut final_block = [0; 100];
    let before = compressor.total_out();
    compressor.compress(&[], &mut final_block, FlushCompress::Finish)?;
    let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
    gzip.write_all(data)?;
    let gzip = gzip.finish()?;
    let mut out = vec![31, 139, 8, 4, 0, 0, 0, 0, 0, 3];
    let size = 6 + blocks.len() * 2;
    out.extend_from_slice(&((size + 4) as u16).to_le_bytes());
    out.extend_from_slice(b"RA");
    out.extend_from_slice(&(size as u16).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(chunk as u16).to_le_bytes());
    out.extend_from_slice(&(blocks.len() as u16).to_le_bytes());
    for b in &blocks {
        out.extend_from_slice(&(b.len() as u16).to_le_bytes());
    }
    for b in blocks {
        out.extend_from_slice(&b);
    }
    out.extend_from_slice(&final_block[..(compressor.total_out() - before) as usize]);
    out.extend_from_slice(&gzip[gzip.len() - 8..]);
    let mut check = Vec::new();
    GzDecoder::new(&out[..]).read_to_end(&mut check)?;
    assert_eq!(check, data);
    Ok(out)
}
// Compare readable content in the existing storage/cache tests; parser tests
// below inspect block kinds, depth and style ranges separately.
fn contents(entries: &[Entry]) -> String {
    entries
        .iter()
        .flat_map(|e| std::iter::once(e.headword.as_str()).chain(e.blocks.iter().map(|b| b.text.as_str())))
        .collect::<Vec<_>>()
        .join("\n\n")
}
#[test]
fn offsets_compression_homographs_unicode_synonyms_and_cache_reopen() -> Result<()> {
    for wide in [false, true] {
        for compressed in [false, true] {
            let f = Fixture::new();
            f.install(wide, compressed)?;
            let mut backend = f.backend()?;
            let exact = contents(&backend.lookup("apple")?);
            assert!(exact.starts_with("apple\n\na lowercase entry"));
            assert!(exact.contains("A fruit.") && exact.contains("A tree."));
            assert!(contents(&backend.lookup("FRUIT")?).contains("Apple\n\nA fruit."));
            assert!(contents(&backend.lookup("СЛОВО")?).contains("Русское определение."));
            assert!(contents(&backend.lookup("CAFE\u{301}")?).contains("Coffee & cake\n\nAnother paragraph."));
            assert!(contents(&backend.lookup("missing")?).is_empty());
            let stamps: Vec<_> =
                fs::read_dir(f.0.join("cache"))?.map(|e| e.unwrap().metadata().unwrap().modified().unwrap()).collect();
            drop(backend);
            assert!(contents(&f.backend()?.lookup("Apple")?).contains("A tree."));
            let restored: Vec<_> =
                fs::read_dir(f.0.join("cache"))?.map(|e| e.unwrap().metadata().unwrap().modified().unwrap()).collect();
            assert_eq!(stamps, restored, "valid disk index is reused");
        }
    }
    Ok(())
}
#[test]
fn long_utf8_headwords_and_aliases_survive_build_lookup_and_cache_reopen() -> Result<()> {
    for wide in [false, true] {
        for compressed in [false, true] {
            let f = Fixture::new();
            let long = "я".repeat(2048); // Exactly 4096 bytes, excluding the NUL.
            let alias = "ю".repeat(2048);
            f.install_words(wide, compressed, &[(&long, "Long definition"), ("short", "Short definition")])?;
            let mut syn = alias.as_bytes().to_vec();
            syn.extend_from_slice(&[0, 0, 0, 0, 0]);
            fs::write(f.path().with_extension("syn"), syn)?;
            let mut backend = f.backend()?;
            for query in [&long, &alias] {
                let entries = backend.lookup(query)?;
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].headword, long);
            }
            drop(backend);
            let cache = f.cache_files()?.pop().unwrap().0;
            // Holding the inode open proves reopen did not replace it with a
            // newly built, byte-identical cache.
            use std::os::unix::fs::MetadataExt;
            let held = fs::File::open(&cache)?;
            assert_eq!(f.backend()?.lookup(&alias)?[0].headword, long);
            assert_eq!(held.metadata()?.ino(), fs::metadata(cache)?.ino());
        }
    }
    Ok(())
}

#[test]
fn malformed_records_report_source_record_offset_and_cause_without_a_cache() -> Result<()> {
    for ext in ["idx", "syn"] {
        for wide in [false, true] {
            for compressed in [false, true] {
                for (word, tail, cause) in [
                    ("a".repeat(4097), vec![0; 12], "Headword too long"),
                    ("я".repeat(2049), vec![0; 12], "Headword too long"),
                    ("missing terminator".into(), vec![], "Unterminated headword"),
                    (
                        "x\0".into(),
                        vec![
                            0;
                            if ext == "syn" {
                                3
                            } else if wide {
                                11
                            } else {
                                7
                            }
                        ],
                        "Truncated",
                    ),
                    // U+0130 folds from 2 bytes to 3: the source fits, its key doesn't.
                    ("İ".repeat(1366), vec![0; 13], "Folded headword too long"),
                ] {
                    let f = Fixture::new();
                    f.install_words(wide, compressed, &[("ok", "definition"), ("second", "definition")])?;
                    if ext == "syn" {
                        let info = fs::read_to_string(f.path())?.replace("synwordcount=1", "synwordcount=2");
                        fs::write(f.path(), info)?;
                    }
                    let mut data = b"ok\0".to_vec();
                    data.extend_from_slice(&vec![
                        0;
                        if ext == "syn" {
                            4
                        } else if wide {
                            12
                        } else {
                            8
                        }
                    ]);
                    let offset = data.len();
                    data.extend_from_slice(word.as_bytes());
                    data.extend_from_slice(&tail);
                    // Long terminated words need a NUL before a valid zero tail;
                    // all-zero tails above already include that NUL.
                    let path = f.path().with_extension(if ext == "idx" && compressed { "idx.gz" } else { ext });
                    fs::write(&path, if ext == "idx" && compressed { gzip(&data)? } else { data })?;
                    let error = match f.backend() {
                        Ok(_) => panic!("accepted {ext} {cause}"),
                        Err(e) => format!("{e:#}"),
                    };
                    assert!(error.contains(&path.display().to_string()), "{error}");
                    assert!(error.contains(&format!(".{ext} record 1 at byte offset {offset}")), "{error}");
                    assert!(error.contains(cause), "{error}");
                    assert!(fs::read_dir(f.0.join("cache"))?.next().is_none(), "failed build published files");
                }
            }
        }
    }
    Ok(())
}

#[test]
fn legacy_v1_cache_opens_without_reading_or_rebuilding_source_records() -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let f = Fixture::new();
    f.install(false, false)?;
    // Build a v1 cache with the same short records accepted before Phase 2.
    drop(f.backend()?);
    let cache = f.cache_files()?.pop().unwrap().0;
    let held = fs::File::open(&cache)?;
    use sha2::{Digest, Sha256};
    let mut signature = Sha256::new();
    signature.update(b"verso-stardict-index-v1");
    for ext in ["ifo", "idx", "dict", "syn"] {
        let source = f.path().with_extension(ext);
        let meta = fs::metadata(&source)?;
        signature.update(source.as_os_str().as_encoded_bytes());
        signature.update(meta.len().to_le_bytes());
        signature.update(meta.modified()?.duration_since(std::time::UNIX_EPOCH)?.as_nanos().to_le_bytes());
    }
    assert_eq!(&fs::read(&cache)?[..64], format!("{:x}", signature.finalize()).as_bytes(), "legacy v1 signature");
    let source = f.path().with_extension("idx");
    let meta = fs::metadata(&source)?;
    // Corrupt the source without changing its length or timestamp (the v1
    // signature). A rebuild must fail, so a successful lookup proves reuse.
    fs::write(&source, vec![b'x'; meta.len() as usize])?;
    fs::File::options().write(true).open(&source)?.set_modified(meta.modified()?)?;
    assert!(contents(&f.backend()?.lookup("Apple")?).contains("A fruit."));
    assert_eq!(held.metadata()?.ino(), fs::metadata(cache)?.ino());
    Ok(())
}

/// Run the backend directly on installed, read-only dictionary files. The
/// caller supplies an isolated writable cache and a UTF-8 query file; this
/// does not initialize the display/input backend or touch reader settings.
#[test]
#[ignore = "requires VERSO_TEST_IFO, VERSO_TEST_CACHE and VERSO_TEST_QUERY_FILE"]
fn installed_dictionary_lookup() -> Result<()> {
    let path = PathBuf::from(std::env::var("VERSO_TEST_IFO")?);
    let cache = PathBuf::from(std::env::var("VERSO_TEST_CACHE")?);
    let query = fs::read_to_string(std::env::var("VERSO_TEST_QUERY_FILE")?)?;
    for _ in 0..2 {
        let entries = Backend::open(&path, &cache, &|| true)?.lookup(&query)?;
        ensure!(!entries.is_empty(), "Installed dictionary lookup missed");
        ensure!(entries.iter().any(|e| e.blocks.iter().any(|b| !b.text.is_empty())), "No readable definition");
        println!("{}-byte query: {} readable entries", query.len(), entries.len());
    }
    Ok(())
}

#[test]
fn reads_beyond_four_gib_without_mapping_definitions() -> Result<()> {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join("dictionaries"))?;
    let offset = u32::MAX as u64 + 123;
    let mut data = fs::File::create(f.path().with_extension("dict"))?;
    data.seek(SeekFrom::Start(offset))?;
    data.write_all(b"large offset")?;
    let mut idx = b"word\0".to_vec();
    idx.extend_from_slice(&offset.to_be_bytes());
    idx.extend_from_slice(&12u32.to_be_bytes());
    fs::write(f.path().with_extension("idx"), &idx)?;
    fs::write(
        f.path(),
        format!(
            "StarDict's dict ifo file\nversion=3.0.0\nbookname=Wide\nwordcount=1\nidxfilesize={}\nidxoffsetbits=64\nsametypesequence=m\n",
            idx.len()
        ),
    )?;
    assert_eq!(contents(&f.backend()?.lookup("word")?), "word\n\nlarge offset");
    Ok(())
}
#[test]
fn random_access_crosses_chunks_and_cache_stays_bounded() -> Result<()> {
    let f = Fixture::new();
    let data = (0..200000).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let path = f.0.join("data.dz");
    fs::write(&path, dictzip(&data, 4096)?)?;
    let mut source = Definitions::open(&path, true)?;
    for offset in [190000, 4000, 65530, 0, 199999] {
        let size = 5000.min(data.len() - offset);
        assert_eq!(source.read(offset as u64, size as u32)?, data[offset..offset + size]);
    }
    assert!(source.read(199999, 2).is_err());
    assert!(source.read(0, 1024 * 1024 + 1).is_err());
    // A corrupt distant chunk cannot prevent reading the first block.
    let mut zip = fs::read(&path)?;
    let distant = zip.len() - 200;
    zip[distant] ^= 0xff;
    fs::write(&path, zip)?;
    let mut source = Definitions::open(&path, true)?;
    assert_eq!(source.read(0, 10)?, data[..10]);
    Ok(())
}
#[test]
fn discovery_preferences_missing_broken_and_rescan() -> Result<()> {
    let f = Fixture::new();
    let root = f.0.join("dictionaries");
    assert!(discover(&root, &[])?.is_empty());
    f.install(false, false)?;
    fs::create_dir_all(root.join("nested"))?;
    fs::write(root.join("nested/bad.ifo"), b"bad")?;
    let prefs = [Preference { id: "test.ifo".into(), disabled: true }];
    let found = discover(&root, &prefs)?;
    assert_eq!(found.len(), 2);
    assert!(found[0].disabled);
    assert!(found[1].error.is_some());
    let _ = f.backend()?;
    fs::write(f.path().with_extension("syn"), b"fruit\0\0\0\0\xff")?;
    assert!(f.backend().is_err(), "changed files invalidate disk cache");
    Ok(())
}
#[test]
fn malformed_truncated_and_cancelled_sources_fail_without_publishing_cache() -> Result<()> {
    for broken in ["idx", "dict", "syn", "ifo"] {
        let f = Fixture::new();
        f.install(false, false)?;
        fs::write(f.path().with_extension(broken), b"broken")?;
        assert!(f.backend().is_err(), "{broken}");
    }
    let f = Fixture::new();
    f.install(false, true)?;
    fs::write(f.path().with_extension("idx.gz"), b"bad gzip")?;
    assert!(f.backend().is_err());
    let f = Fixture::new();
    f.install(false, false)?;
    assert!(Backend::open(&f.path(), &f.0.join("cache"), &|| false).is_err());
    assert!(fs::read_dir(f.0.join("cache"))?.next().is_none());
    assert!(contents(&f.backend()?.lookup("Apple")?).contains("A fruit."));
    Ok(())
}
#[test]
fn worker_latest_reply_preferences_and_errors() -> Result<()> {
    use crate::{
        dictionary::worker::{State, Worker},
        platform::Waker,
        store::Store,
    };
    use std::{sync::mpsc, time::Duration};
    let f = Fixture::new();
    f.install(false, true)?;
    let store = Store::at(f.0.clone());
    let (tx, rx) = mpsc::channel();
    let worker = Worker::new(
        store.clone(),
        Waker::new(move || {
            let _ = tx.send(());
        }),
    );
    worker.lookup("old".into(), None, true);
    let latest = worker.lookup("FRUIT".into(), None, false);
    let receive = || -> Result<worker::Reply> {
        loop {
            if let Some(reply) = worker.receive() {
                return Ok(reply);
            }
            rx.recv_timeout(Duration::from_secs(5))?;
        }
    };
    loop {
        let r = receive()?;
        if r.id == latest {
            assert!(matches!(r.state, Some(State::Definition { .. })));
            break;
        }
    }
    worker.change("test.ifo".into(), false);
    let latest = worker.lookup("fruit".into(), None, false);
    let reply = receive()?;
    assert_eq!(reply.id, latest);
    assert_eq!(reply.state, Some(State::Disabled));
    let prefs: Vec<Preference> = serde_json::from_str(&fs::read_to_string(store.dictionary_preferences())?)?;
    assert!(prefs[0].disabled);
    drop(worker);
    let worker = Worker::new(store.clone(), Waker::new(|| {}));
    let id = worker.lookup("fruit".into(), None, true);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(reply) = worker.receive() {
            assert_eq!(reply.id, id);
            assert_eq!(reply.state, Some(State::Disabled));
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
#[test]
fn basename_strips_both_metadata_suffixes() {
    assert_eq!(basename(Path::new("a/en.v2.ifo")), Some(PathBuf::from("a/en.v2")));
    assert_eq!(basename(Path::new("a/en.v2.ifo.gz")), Some(PathBuf::from("a/en.v2")));
    assert_eq!(sibling(Path::new("a/en.v2"), "idx.gz"), PathBuf::from("a/en.v2.idx.gz"));
    for other in ["a/en.idx.gz", "a/.ifo", "a/.ifo.gz", "a/en.gz", "a/en"] {
        assert_eq!(basename(Path::new(other)), None, "{other}");
    }
}
#[test]
fn compressed_metadata_alone_with_plain_and_compressed_companions() -> Result<()> {
    for compressed in [false, true] {
        let f = Fixture::new();
        f.install(false, compressed)?;
        let gz = f.gzip_ifo(false)?;
        let found = discover(&f.0.join("dictionaries"), &[])?;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].error, None);
        assert_eq!(found[0].name, "Test dictionary");
        assert_eq!(found[0].path, gz);
        assert_eq!(found[0].id, "test.ifo", "same ID as a plain .ifo install");
        let mut backend = Backend::open(&gz, &f.0.join("cache"), &|| true)?;
        assert!(contents(&backend.lookup("FRUIT")?).contains("Apple\n\nA fruit."));
        assert!(contents(&backend.lookup("apple")?).starts_with("apple\n\na lowercase entry"));
    }
    Ok(())
}
#[test]
fn both_metadata_variants_list_once_and_prefer_plain() -> Result<()> {
    let f = Fixture::new();
    f.install(false, true)?;
    let gz = f.path().with_extension("ifo.gz");
    let text = fs::read_to_string(f.path())?.replace("Test dictionary", "Compressed copy");
    fs::write(&gz, gzip(text.as_bytes())?)?;
    let prefs = [Preference { id: "test.ifo".into(), disabled: true }];
    let found = discover(&f.0.join("dictionaries"), &prefs)?;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, f.path());
    assert_eq!(found[0].name, "Test dictionary");
    assert!(found[0].disabled);
    Ok(())
}
#[test]
fn corrupt_truncated_and_oversized_compressed_metadata_fail_per_dictionary() -> Result<()> {
    let header = |f: &Fixture| fs::read(f.path());
    type Make = fn(Vec<u8>) -> Result<Vec<u8>>;
    let cases: [(&str, Make); 4] = [
        ("corrupt", |_| Ok(b"not gzip at all".to_vec())),
        ("truncated", |ifo| {
            let z = gzip(&ifo)?;
            Ok(z[..z.len() - 12].to_vec())
        }),
        ("bad checksum", |ifo| {
            let mut z = gzip(&ifo)?;
            let at = z.len() - 6;
            z[at] ^= 0xff;
            Ok(z)
        }),
        ("oversized", |mut ifo| {
            ifo.extend(std::iter::repeat_n(b'#', 70000));
            gzip(&ifo)
        }),
    ];
    for (label, make) in cases {
        let f = Fixture::new();
        f.install(false, true)?;
        let gz = f.path().with_extension("ifo.gz");
        let data = make(header(&f)?)?;
        if label == "oversized" {
            assert!(data.len() < 65536, "compressed size is small, decompressed is not");
        }
        fs::write(&gz, data)?;
        fs::remove_file(f.path())?;
        let root = f.0.join("dictionaries");
        fs::create_dir_all(root.join("other"))?;
        let other = Fixture::new();
        other.install(false, false)?;
        for ext in ["ifo", "idx", "dict", "syn"] {
            fs::copy(other.path().with_extension(ext), root.join("other/good").with_extension(ext))?;
        }
        let found = discover(&root, &[])?;
        assert_eq!(found.len(), 2, "{label}");
        let broken = found.iter().find(|d| d.path == gz).unwrap();
        let error = broken.error.as_deref().unwrap_or_else(|| panic!("{label} has no error"));
        assert!(error.contains(".ifo.gz"), "{label}: {error}");
        assert_eq!(broken.name, "test");
        assert!(found.iter().any(|d| d.path.ends_with("good.ifo") && d.error.is_none()), "{label}");
        assert!(Backend::open(&gz, &f.0.join("cache"), &|| true).is_err(), "{label}");
    }
    Ok(())
}
#[test]
fn replacing_either_metadata_variant_invalidates_cache() -> Result<()> {
    let f = Fixture::new();
    f.install(false, true)?;
    assert!(contents(&f.backend()?.lookup("Apple")?).contains("A tree."));
    let plain = f.cache_files()?;
    assert_eq!(plain.len(), 1);
    // Plain -> compressed: the cache file is reused by name but rebuilt.
    let gz = f.gzip_ifo(false)?;
    let open = || Backend::open(&gz, &f.0.join("cache"), &|| true);
    assert!(contents(&open()?.lookup("Apple")?).contains("A tree."));
    let compressed = f.cache_files()?;
    assert_eq!(compressed.len(), 1);
    assert_eq!(compressed[0].0, plain[0].0);
    assert_ne!(compressed[0].1, plain[0].1);
    // A valid cache is reused as long as the .ifo.gz is unchanged.
    assert_eq!(f.cache_files()?, compressed);
    let _ = open()?;
    assert_eq!(f.cache_files()?, compressed);
    // Replacing the .ifo.gz itself is detected.
    let mut text = String::new();
    GzDecoder::new(&fs::read(&gz)?[..]).read_to_string(&mut text)?;
    fs::write(&gz, gzip(text.replace("wordcount=5", "wordcount=4").as_bytes())?)?;
    assert!(open().is_err(), "stale cache is not reused after replacement");
    // Back to plain metadata rebuilds again.
    fs::write(f.path(), text)?;
    assert!(contents(&f.backend()?.lookup("FRUIT")?).contains("A fruit."));
    assert_ne!(f.cache_files()?[0].1, compressed[0].1);
    Ok(())
}

#[test]
fn base_forms_choose_exceptions_and_first_rule_with_bounded_headwords() -> Result<()> {
    for compressed in [false, true] {
        let f = Fixture::new();
        f.install_words(
            false,
            compressed,
            &[
                ("axe", "A tool."),
                ("axis", "A line."),
                ("child", "A young person."),
                ("go", "Move."),
                ("hop", "Jump."),
                ("hope", "Expect."),
                ("kiss", "Touch with lips."),
                ("run", "Move fast."),
                ("study", "Learn."),
                ("thunder", "A loud sound."),
                ("tree", "A plant."),
                ("us", "A pronoun."),
                ("use", "Employ."),
            ],
        )?;
        let mut backend = f.backend()?;
        for (query, expected) in [
            ("uses", vec!["use"]),
            ("hoping", vec!["hope"]),
            ("axes", vec!["axe", "axis"]),
            ("thunders", vec!["thunder"]),
            ("studies", vec!["study"]),
            ("running", vec!["run"]),
            ("went", vec!["go"]),
            ("kissed", vec!["kiss"]),
            ("trees", vec!["tree"]),
            ("children’s", vec!["child"]),
            ("tree's", vec!["tree"]),
        ] {
            assert_eq!(contents(&backend.lookup(query)?), "", "exact lookup of {query}");
            let (text, count) = backend.lookup_forms(&forms::candidates(query).unwrap(), &|| true)?;
            let headwords: Vec<_> = text.iter().map(|e| e.headword.as_str()).collect();
            assert_eq!(headwords, expected, "{query}");
            assert_eq!(count, expected.len(), "{query}");
        }
        // The intact possessive wins even if more exceptions/rules exist.
        let candidates = forms::Candidates {
            possessive: Some("tree".into()),
            exceptions: vec!["child"],
            rules: vec!["thunder".into()],
        };
        let (text, count) = backend.lookup_forms(&candidates, &|| true)?;
        assert_eq!((contents(&text).as_str(), count), ("tree\n\nA plant.", 1));
        // Repeated lemmas and a synonym sharing a definition span do not count
        // twice; collect all exceptions before the first rule, capped at three.
        let candidates = forms::Candidates {
            possessive: None,
            exceptions: vec!["axe", "axe", "fruit", "axis", "child"],
            rules: vec!["thunder".into()],
        };
        let (text, count) = backend.lookup_forms(&candidates, &|| true)?;
        assert_eq!(count, 3);
        assert_eq!(contents(&text), "axe\n\nA tool.\n\naxis\n\nA line.\n\nchild\n\nA young person.");
        // `candidates` no longer combines exceptions and rules, but the backend
        // still treats a rule matching an exception's span as the first rule hit.
        let candidates =
            forms::Candidates { possessive: None, exceptions: vec!["use"], rules: vec!["use".into(), "us".into()] };
        let (entries, count) = backend.lookup_forms(&candidates, &|| true)?;
        assert_eq!((contents(&entries), count), ("use\n\nEmploy.".into(), 1));
    }
    Ok(())
}

#[test]
fn base_form_count_ignores_case_variants_of_one_headword() -> Result<()> {
    let f = Fixture::new();
    f.install(false, false)?;
    let mut backend = f.backend()?;
    // `apple` and `Apple` are one base form, shown with all three entries.
    let (text, count) = backend.lookup_forms(&forms::candidates("apples").unwrap(), &|| true)?;
    assert_eq!(count, 1);
    assert_eq!(contents(&text), "apple\n\na lowercase entry\n\nApple\n\nA fruit.\n\nApple\n\nA tree.");
    // Case variants don't use up the cap either.
    let candidates =
        forms::Candidates { possessive: None, exceptions: vec!["apple", "café", "слово"], rules: Vec::new() };
    let (text, count) = backend.lookup_forms(&candidates, &|| true)?;
    assert_eq!(count, 3);
    assert!(contents(&text).ends_with("Слово\n\nРусское определение."), "{text:?}");
    Ok(())
}

#[test]
fn base_forms_cancel_between_candidates_before_reading_definitions() -> Result<()> {
    use std::cell::Cell;
    let f = Fixture::new();
    f.install(false, false)?;
    let mut backend = f.backend()?;
    let checks = Cell::new(0);
    let current = || {
        let n = checks.get();
        checks.set(n + 1);
        n < 1
    };
    let candidates = forms::Candidates { possessive: None, exceptions: vec!["Apple", "missing"], rules: Vec::new() };
    assert!(backend.lookup_forms(&candidates, &current).is_err());
    assert_eq!(checks.get(), 2);
    Ok(())
}

#[test]
fn combined_output_is_capped_on_utf8_boundaries_with_an_omission_notice() -> Result<()> {
    let f = Fixture::new();
    let meaning = "<b>Ж</b>".repeat(100_000);
    // A record stays under 1 MiB; six homographs exceed the combined limit.
    f.install_words(false, false, &[("word", meaning.as_str()); 6])?;
    let entries = f.backend()?.lookup("word")?;
    assert_eq!(entries.len(), 6);
    let blocks: Vec<_> = entries.iter().flat_map(|e| &e.blocks).collect();
    assert!(blocks.iter().map(|b| b.text.len()).sum::<usize>() <= 1024 * 1024);
    assert_eq!(blocks.last().unwrap().text, "[Further entries omitted]");
    // When a multibyte character cannot fit, later ASCII blocks must not
    // sneak into the retained prefix.
    let f = Fixture::new();
    let prefix = "x".repeat(1024 * 1024 - "[Further entries omitted]".len() - 1);
    f.install_words(false, false, &[("word", &prefix), ("word", "<p>Ж</p><p>tail</p>")])?;
    let clipped = f.backend()?.lookup("word")?;
    assert_eq!(clipped[1].blocks, vec![Block::plain("[Further entries omitted]")]);
    // No gap is left between the retained text and the notice.
    let f = Fixture::new();
    f.install_words(false, false, &[("word", &prefix[1..]), ("word", "<p>a</p><p></p><p>Ж</p>")])?;
    let clipped = f.backend()?.lookup("word")?;
    let texts: Vec<_> = clipped[1].blocks.iter().map(|b| (b.kind, b.text.as_str())).collect();
    assert_eq!(texts, [(BlockKind::Text, "a"), (BlockKind::Text, "[Further entries omitted]")]);
    for b in blocks {
        for s in &b.spans {
            assert!(s.start < s.end && s.end as usize <= b.text.len());
            assert!(b.text.is_char_boundary(s.start as usize) && b.text.is_char_boundary(s.end as usize));
        }
    }
    Ok(())
}
