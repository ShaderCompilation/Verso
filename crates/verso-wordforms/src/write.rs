//! Writes `.vwf` packs (see the crate docs for the layout).
use super::{Encoding, HEADER, MAGIC, MAX_LEMMA_BYTES, MAX_LEMMAS, VERSION, put_varint};
use anyhow::{Context, Result, ensure};
use std::collections::HashMap;

/// Sizes of a finished pack, in bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub forms: u64,
    /// Distinct values: lemma lists or rule lists.
    pub values: u64,
    pub meta_bytes: u64,
    pub value_bytes: u64,
    pub string_bytes: u64,
    pub map_bytes: u64,
    pub total_bytes: u64,
}

/// Collects forms in byte order of their folded keys, then writes the pack.
pub struct Writer {
    encoding: Encoding,
    meta: Vec<u8>,
    map: fst::MapBuilder<Vec<u8>>,
    values: Vec<u8>,
    value_offsets: HashMap<Vec<u8>, u64>,
    strings: Vec<u8>,
    string_offsets: HashMap<String, u64>,
    scratch: Vec<u8>,
    forms: u64,
}

impl Writer {
    /// `meta` pairs must not contain tabs or line breaks.
    pub fn new(encoding: Encoding, meta: &[(&str, &str)]) -> Result<Self> {
        let mut text = String::new();
        for (key, value) in meta {
            ensure!(![key, value].iter().any(|s| s.contains(['\t', '\n', '\r'])), "Invalid pack metadata {key}");
            text.push_str(&format!("{key}\t{value}\n"));
        }
        Ok(Self {
            encoding,
            meta: text.into_bytes(),
            map: fst::MapBuilder::memory(),
            values: Vec::new(),
            value_offsets: HashMap::new(),
            strings: Vec::new(),
            string_offsets: HashMap::new(),
            scratch: Vec::new(),
            forms: 0,
        })
    }

    /// Adds a form. `key` must already be folded (see [`super::fold`]) and
    /// come after the previous key in byte order; `lemmas` are best first.
    pub fn insert(&mut self, key: &str, lemmas: &[&str]) -> Result<()> {
        ensure!((1..=MAX_LEMMAS).contains(&lemmas.len()), "{key}: {} lemmas (1–{MAX_LEMMAS} allowed)", lemmas.len());
        self.scratch.clear();
        put_varint(&mut self.scratch, lemmas.len() as u64);
        for lemma in lemmas {
            ensure!(!lemma.is_empty() && lemma.len() <= MAX_LEMMA_BYTES, "{key}: invalid lemma {lemma:?}");
            match self.encoding {
                Encoding::Sets => {
                    let offset = match self.string_offsets.get(*lemma) {
                        Some(&offset) => offset,
                        None => {
                            let offset = self.strings.len() as u64;
                            put_varint(&mut self.strings, lemma.len() as u64);
                            self.strings.extend_from_slice(lemma.as_bytes());
                            self.string_offsets.insert((*lemma).to_owned(), offset);
                            offset
                        }
                    };
                    put_varint(&mut self.scratch, offset);
                }
                Encoding::Rules => {
                    let (cut, suffix) = rule(key, lemma);
                    put_varint(&mut self.scratch, cut as u64);
                    put_varint(&mut self.scratch, suffix.len() as u64);
                    self.scratch.extend_from_slice(suffix.as_bytes());
                }
            }
        }
        let value = match self.value_offsets.get(&self.scratch) {
            Some(&offset) => offset,
            None => {
                let offset = self.values.len() as u64;
                self.values.extend_from_slice(&self.scratch);
                self.value_offsets.insert(self.scratch.clone(), offset);
                offset
            }
        };
        self.map.insert(key, value).with_context(|| format!("Form {key:?} out of order"))?;
        self.forms += 1;
        Ok(())
    }

    /// The finished pack.
    pub fn finish(self) -> Result<(Vec<u8>, Stats)> {
        let forms = self.forms;
        let map = self.map.into_inner()?;
        let sections = [&self.meta[..], &self.values, &self.strings, &map];
        let mut out = Vec::with_capacity(HEADER + sections.iter().map(|s| s.len()).sum::<usize>());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(self.encoding as u32).to_le_bytes());
        let mut offset = HEADER as u64;
        for section in sections {
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&(section.len() as u64).to_le_bytes());
            offset += section.len() as u64;
        }
        for section in sections {
            out.extend_from_slice(section);
        }
        let stats = Stats {
            forms,
            values: self.value_offsets.len() as u64,
            meta_bytes: self.meta.len() as u64,
            value_bytes: self.values.len() as u64,
            string_bytes: self.strings.len() as u64,
            map_bytes: map.len() as u64,
            total_bytes: out.len() as u64,
        };
        Ok((out, stats))
    }
}

/// How to turn `key` into `lemma`: characters to cut from its end and the
/// suffix to append, after their common prefix.
fn rule<'a>(key: &str, lemma: &'a str) -> (usize, &'a str) {
    let common = key
        .char_indices()
        .zip(lemma.chars())
        .take_while(|((_, a), b)| a == b)
        .last()
        .map_or(0, |((i, c), _)| i + c.len_utf8());
    (key[common..].chars().count(), &lemma[common..])
}
