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
        fs::create_dir_all(self.0.join("dictionaries"))?;
        let mut idx = Vec::new();
        let mut data = Vec::new();
        for (word, meaning) in [
            ("Apple", "A fruit."),
            ("Apple", "A tree."),
            ("apple", "a lowercase entry"),
            ("café", "<p>Coffee &amp; cake</p><p>Another paragraph.</p>"),
            ("Слово", "Русское определение."),
        ] {
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
                "StarDict's dict ifo file\nversion=3.0.0\nbookname=Test dictionary\nwordcount=5\nsynwordcount=1\nidxfilesize={}\nidxoffsetbits={}\nsametypesequence=h\n",
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
#[test]
fn offsets_compression_homographs_unicode_synonyms_and_cache_reopen() -> Result<()> {
    for wide in [false, true] {
        for compressed in [false, true] {
            let f = Fixture::new();
            f.install(wide, compressed)?;
            let mut backend = f.backend()?;
            let exact = backend.lookup("apple")?;
            assert!(exact.starts_with("apple\n\na lowercase entry"));
            assert!(exact.contains("A fruit.") && exact.contains("A tree."));
            assert!(backend.lookup("FRUIT")?.contains("Apple\n\nA fruit."));
            assert!(backend.lookup("СЛОВО")?.contains("Русское определение."));
            assert!(backend.lookup("CAFE\u{301}")?.contains("Coffee & cake\n\nAnother paragraph."));
            assert!(backend.lookup("missing")?.is_empty());
            let stamps: Vec<_> =
                fs::read_dir(f.0.join("cache"))?.map(|e| e.unwrap().metadata().unwrap().modified().unwrap()).collect();
            drop(backend);
            assert!(f.backend()?.lookup("Apple")?.contains("A tree."));
            let restored: Vec<_> =
                fs::read_dir(f.0.join("cache"))?.map(|e| e.unwrap().metadata().unwrap().modified().unwrap()).collect();
            assert_eq!(stamps, restored, "valid disk index is reused");
        }
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
    assert_eq!(f.backend()?.lookup("word")?, "word\n\nlarge offset");
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
    assert!(f.backend()?.lookup("Apple")?.contains("A fruit."));
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
            assert!(matches!(r.state, State::Definition(_)));
            break;
        }
    }
    worker.change("test.ifo".into(), false);
    let latest = worker.lookup("fruit".into(), None, false);
    let reply = receive()?;
    assert_eq!(reply.id, latest);
    assert_eq!(reply.state, State::Disabled);
    let prefs: Vec<Preference> = serde_json::from_str(&fs::read_to_string(store.dictionary_preferences())?)?;
    assert!(prefs[0].disabled);
    drop(worker);
    let worker = Worker::new(store.clone(), Waker::new(|| {}));
    let id = worker.lookup("fruit".into(), None, true);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(reply) = worker.receive() {
            assert_eq!(reply.id, id);
            assert_eq!(reply.state, State::Disabled);
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}
