//! Word-form packs (`.vwf`): a memory-mapped map from a word form to its
//! lemmas, so the dictionary can turn `кошки` into `кошка` and `шёл` into
//! `идти`. The app only reads packs; the `build` feature adds the OpenCorpora
//! reader and the writer used by `vwf-build`.
//!
//! Layout (all integers little endian):
//!
//! | Bytes | Field |
//! |---|---|
//! | 0..8 | magic `VERSOVWF` |
//! | 8..12 | format version ([`VERSION`]) |
//! | 12..16 | value encoding ([`Encoding`]) |
//! | 16..80 | four sections, each a `u64` offset and `u64` length: meta, values, strings, form map |
//!
//! - **meta**: UTF-8 `key\tvalue` lines (`language`, `source`, `revision`, `date`).
//! - **form map**: an `fst` map from the [`fold`]ed form to a byte offset in
//!   the values section.
//! - **values**: at that offset, a LEB128 count of lemmas, then per lemma
//!   either a LEB128 offset into the strings section ([`Encoding::Sets`]) or a
//!   rule ([`Encoding::Rules`]): a LEB128 count of characters cut from the end
//!   of the form, then a length-prefixed suffix to append.
//! - **strings**: length-prefixed UTF-8 lemmas.
use anyhow::{Context, Result, bail, ensure};
use caseless::Caseless;
use std::{fs::File, path::Path, sync::Arc};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

#[cfg(feature = "build")]
pub mod opencorpora;
#[cfg(test)]
mod tests;
#[cfg(feature = "build")]
pub mod write;

pub const MAGIC: &[u8; 8] = b"VERSOVWF";
/// Bump when the layout or [`fold`] changes; older packs are then refused.
pub const VERSION: u32 = 1;
const HEADER: usize = 80;
/// The longest word a pack looks up, before folding.
pub const MAX_WORD_BYTES: usize = 128;
/// The most lemmas one form can have; the writer refuses more and the reader
/// treats more as corruption.
pub const MAX_LEMMAS: usize = 32;
/// The longest lemma a pack stores.
pub const MAX_LEMMA_BYTES: usize = 512;

/// How a form's lemmas are stored in the values section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Lemma strings stored once, values list their offsets.
    Sets = 1,
    /// Each lemma is rebuilt from the form: cut its end, append a suffix.
    Rules = 2,
}

impl Encoding {
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Sets => "sets",
            Encoding::Rules => "rules",
        }
    }
}

/// The dictionary key for a headword, query or word form: case folded and
/// normalized like the library search, and also blind to Russian ё/е and
/// stress marks, so `ещё` finds `еще` and `молоко́` finds `молоко`. Marks are
/// judged on the decomposed text, against the letter they follow:
/// - acute and grave (U+0301, U+0300) after a Cyrillic letter are stress
///   marks; dropping the grave too keeps `ѐ` and `ѝ` from recomposing into
///   separate letters;
/// - the diaeresis (U+0308) goes only after `е`, so Ukrainian `ї` keeps it;
/// - `й`'s breve and every mark on Latin letters stay.
///
/// Shared by the dictionary index and the packs: when it changes, bump the
/// index `KEY_VERSION` in the app and [`VERSION`] here.
pub fn fold(text: &str) -> String {
    let mut out = String::new();
    fold_into(text, &mut out);
    out
}

/// [`fold`] into a reused buffer, replacing its contents. Index builds fold
/// every headword of every dictionary, so the common text skips the Unicode
/// pipeline: ASCII and the basic Russian alphabet have no marks to judge and
/// fold one character to one. Anything else takes the full pipeline; a test
/// checks the shortcut against it for every BMP character.
pub fn fold_into(text: &str, out: &mut String) {
    out.clear();
    if text.is_ascii() {
        out.push_str(text);
        out.make_ascii_lowercase();
    } else if !fold_plain(text, out) {
        out.clear();
        fold_full(text, out);
    }
}

/// The shortcut for ASCII and `А..я`, `Ё`, `ё`; `false` on any other
/// character, leaving `out` partly written.
fn fold_plain(text: &str, out: &mut String) -> bool {
    out.reserve(text.len());
    for c in text.chars() {
        out.push(match c {
            '\0'..='\x7f' => c.to_ascii_lowercase(),
            // Of these, NFD splits only Й and й (и + breve), which recompose.
            'А'..='Я' => char::from_u32(c as u32 + 0x20).unwrap(),
            'а'..='я' => c,
            'Ё' | 'ё' => 'е',
            _ => return false,
        });
    }
    true
}

fn fold_full(text: &str, out: &mut String) {
    let mut base = '\0';
    out.extend(
        text.nfd()
            .default_case_fold()
            .filter(|&c| {
                let drop = match c {
                    '\u{301}' | '\u{300}' => cyrillic(base),
                    '\u{308}' => base == 'е',
                    _ => false,
                };
                if !is_combining_mark(c) {
                    base = c;
                }
                !drop
            })
            .nfc(),
    );
}

fn cyrillic(c: char) -> bool {
    matches!(c as u32, 0x0400..=0x052f | 0x1c80..=0x1c8f | 0xa640..=0xa69f)
}

/// `true` for a single word a pack can know: letters (with their combining
/// marks) and inner single hyphens (`кое-кто`), at most [`MAX_WORD_BYTES`].
pub fn is_word(word: &str) -> bool {
    if word.is_empty() || word.len() > MAX_WORD_BYTES {
        return false;
    }
    // A hyphen or mark can't start the word, and hyphens can't repeat.
    let mut after_hyphen = true;
    for c in word.chars() {
        if c == '-' {
            if after_hyphen {
                return false;
            }
            after_hyphen = true;
        } else if c.is_alphabetic() {
            after_hyphen = false;
        } else if !(is_combining_mark(c) && !after_hyphen) {
            return false;
        }
    }
    !after_hyphen
}

enum Bytes {
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        match self {
            Bytes::Mapped(map) => map,
            Bytes::Owned(bytes) => bytes,
        }
    }
}

/// A range of the pack, shared with the other sections.
#[derive(Clone)]
struct Section {
    bytes: Arc<Bytes>,
    start: usize,
    end: usize,
}

impl AsRef<[u8]> for Section {
    fn as_ref(&self) -> &[u8] {
        &(*self.bytes).as_ref()[self.start..self.end]
    }
}

/// An open pack. Opening reads only the header and the map's root, so it is
/// cheap; memory-mapped pages are read as lookups touch them.
pub struct Pack {
    encoding: Encoding,
    meta: Vec<(String, String)>,
    values: Section,
    strings: Section,
    map: fst::Map<Section>,
}

impl Pack {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("Can't open word forms {}", path.display()))?;
        // SAFETY: packs are read-only files that Verso installs while it isn't
        // running. Every read below is bounds checked against the mapped
        // length, so a corrupt file gives an error, not undefined behaviour.
        let map =
            unsafe { memmap2::Mmap::map(&file) }.with_context(|| format!("Can't map word forms {}", path.display()))?;
        Self::new(Bytes::Mapped(map)).with_context(|| format!("Invalid word forms {}", path.display()))
    }

    /// A pack held in memory, as the writer returns it.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        Self::new(Bytes::Owned(bytes))
    }

    fn new(bytes: Bytes) -> Result<Self> {
        let data = bytes.as_ref();
        ensure!(data.len() >= HEADER && &data[..8] == MAGIC, "Not a word-form pack");
        let version = u32_at(data, 8);
        ensure!(version == VERSION, "Unsupported word-form pack version {version} (this build reads {VERSION})");
        let encoding = match u32_at(data, 12) {
            1 => Encoding::Sets,
            2 => Encoding::Rules,
            other => bail!("Unknown word-form value encoding {other}"),
        };
        let mut ranges = [(0, 0); 4];
        for (i, range) in ranges.iter_mut().enumerate() {
            let start = u64_at(data, 16 + i * 16);
            let len = u64_at(data, 24 + i * 16);
            let end = start.checked_add(len).filter(|&end| start >= HEADER as u64 && end <= data.len() as u64);
            let end = end.context("Word-form pack section out of bounds")?;
            *range = (start as usize, end as usize);
        }
        let (start, end) = ranges[0];
        let meta = std::str::from_utf8(&data[start..end])
            .context("Invalid word-form pack metadata")?
            .lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        let bytes = Arc::new(bytes);
        let section = |(start, end)| Section { bytes: bytes.clone(), start, end };
        let map = fst::Map::new(section(ranges[3])).context("Invalid word-form map")?;
        Ok(Self { encoding, meta, values: section(ranges[1]), strings: section(ranges[2]), map })
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// A metadata value: `language`, `source`, `revision` or `date`.
    pub fn meta(&self, key: &str) -> Option<&str> {
        self.meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// The number of distinct (folded) forms.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// The lemmas of `word`, best first, or `None` when the pack doesn't know
    /// it. The word's own lemma may be among them (`ёж` → `ёж`): callers that
    /// already tried the exact word skip lemmas whose [`fold`] equals the
    /// word's. An error means the pack is corrupt.
    pub fn lemmas(&self, word: &str) -> Result<Option<Vec<String>>> {
        if !is_word(word) {
            return Ok(None);
        }
        let key = fold(word);
        match self.map.get(&key) {
            Some(value) => self.decode(&key, value).map(Some),
            None => Ok(None),
        }
    }

    /// Calls `f` with every form and its lemmas, in key order. For tools:
    /// it reads the whole pack.
    pub fn for_each(&self, mut f: impl FnMut(&str, Vec<String>) -> Result<()>) -> Result<()> {
        use fst::Streamer;
        let mut stream = self.map.stream();
        while let Some((key, value)) = stream.next() {
            let key = std::str::from_utf8(key).context("Invalid word-form key")?;
            f(key, self.decode(key, value)?)?;
        }
        Ok(())
    }

    fn decode(&self, key: &str, value: u64) -> Result<Vec<String>> {
        let values = self.values.as_ref();
        let mut at =
            usize::try_from(value).ok().filter(|&at| at < values.len()).context("Word-form value out of bounds")?;
        let count = varint(values, &mut at)?;
        ensure!((1..=MAX_LEMMAS as u64).contains(&count), "Invalid word-form lemma count {count}");
        let mut lemmas = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let lemma = match self.encoding {
                Encoding::Sets => {
                    let mut string_at = usize::try_from(varint(values, &mut at)?).unwrap_or(usize::MAX);
                    text(self.strings.as_ref(), &mut string_at)?.to_owned()
                }
                Encoding::Rules => {
                    let cut = varint(values, &mut at)?;
                    let suffix = text(values, &mut at)?;
                    let chars = key.chars().count() as u64;
                    ensure!(cut <= chars, "Invalid word-form rule");
                    let keep = key.char_indices().nth((chars - cut) as usize).map_or(key.len(), |(i, _)| i);
                    format!("{}{suffix}", &key[..keep])
                }
            };
            lemmas.push(lemma);
        }
        Ok(lemmas)
    }
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}

/// Reads a LEB128 integer at `*at` and moves past it.
fn varint(data: &[u8], at: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *data.get(*at).context("Truncated word-form value")?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    bail!("Invalid word-form number")
}

/// Reads a length-prefixed UTF-8 string at `*at` and moves past it.
fn text<'a>(data: &'a [u8], at: &mut usize) -> Result<&'a str> {
    let len = usize::try_from(varint(data, at)?).unwrap_or(usize::MAX);
    ensure!(len <= MAX_LEMMA_BYTES, "Word-form lemma too long");
    let end = at.checked_add(len).filter(|&end| end <= data.len()).context("Word-form lemma out of bounds")?;
    let text = std::str::from_utf8(&data[*at..end]).context("Invalid word-form lemma")?;
    *at = end;
    Ok(text)
}

/// Appends `value` as LEB128.
#[cfg(feature = "build")]
fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}
