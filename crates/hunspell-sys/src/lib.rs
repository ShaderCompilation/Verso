//! Hunspell, built from source (`third_party/hunspell`), with hand-written
//! bindings for the few C functions Verso uses and a small safe wrapper for
//! finding a word's base forms (stems).
//!
//! `Hunspell_create` reports no errors: a missing or broken file just gives an
//! empty dictionary. [`Hunspell::open`] therefore checks the files first (they
//! exist, are readable files under [`MAX_FILE_BYTES`], the `.aff` names a
//! supported encoding and the `.dic` starts with a word count) and returns an
//! [`Error`] instead.
//!
//! Hunspell takes and returns words in the dictionary's own encoding. Verso
//! supports UTF-8 and ISO8859-1 (which maps byte for byte to the first 256
//! code points), so no conversion library is needed.

use std::ffi::{CStr, CString, c_char, c_int};
use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::marker::PhantomData;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

/// Raw bindings. `verso_hunspell_create` and `verso_hunspell_stem` are the
/// shim's exception-catching versions of `Hunspell_create` and
/// `Hunspell_stem` (`shim/verso_hunspell.cxx`); the rest is Hunspell's C API
/// (`hunspell.h`).
pub mod ffi {
    use std::ffi::{c_char, c_int};

    #[repr(C)]
    pub struct Hunhandle {
        _private: [u8; 0],
    }

    unsafe extern "C" {
        /// NULL if Hunspell threw (out of memory, for example).
        pub fn verso_hunspell_create(aff: *const c_char, dic: *const c_char) -> *mut Hunhandle;
        /// The number of stems in `*list`, or -1 if Hunspell threw.
        pub fn verso_hunspell_stem(h: *mut Hunhandle, list: *mut *mut *mut c_char, word: *const c_char) -> c_int;
        pub fn Hunspell_destroy(h: *mut Hunhandle);
        pub fn Hunspell_free_list(h: *mut Hunhandle, list: *mut *mut *mut c_char, n: c_int);
        /// The `SET` name from the `.aff` (`ISO8859-1` without one). Owned by
        /// the handle.
        pub fn Hunspell_get_dic_encoding(h: *mut Hunhandle) -> *mut c_char;
        pub fn Hunspell_get_library_version() -> *const c_char;
    }
}

/// The largest `.aff` or `.dic` [`Hunspell::open`] accepts. Hunspell parses
/// the whole `.dic` into memory, about 4 times its size (Polish: 4.7 MB file,
/// 19 MB RAM on the desktop); this keeps a stray huge file from exhausting
/// the Kindle's memory. Phase 4 of docs/universal-languages.md sets the final
/// limit from Kindle measurements.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Hunspell's own limit on the `.dic` word count (`hashmgr.cxx`).
const MAX_WORD_COUNT: u64 = 10_000_000;

/// Hunspell's version, `1.7.6`.
pub fn library_version() -> &'static str {
    // SAFETY: a static NUL-terminated string.
    unsafe { CStr::from_ptr(ffi::Hunspell_get_library_version()) }.to_str().unwrap_or("")
}

/// A dictionary encoding Verso can convert words to and from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    /// ISO8859-1: one byte per code point, U+0000 to U+00FF.
    Latin1,
}

impl Encoding {
    /// The encoding for a `SET` name, the way Hunspell reads it: only the
    /// exact `UTF-8` turns on its UTF-8 mode; 8-bit names are matched
    /// ignoring case and punctuation (`ISO8859-1`, `iso-8859-1`).
    pub fn from_name(name: &str) -> Option<Encoding> {
        if name == "UTF-8" {
            return Some(Encoding::Utf8);
        }
        let folded: String = name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect();
        (folded == "iso88591").then_some(Encoding::Latin1)
    }

    /// The word in this encoding, or None if it has a character the encoding
    /// lacks (or a NUL).
    fn encode(self, word: &str) -> Option<CString> {
        let bytes = match self {
            Encoding::Utf8 => word.as_bytes().to_vec(),
            Encoding::Latin1 => word.chars().map(|c| u8::try_from(u32::from(c)).ok()).collect::<Option<Vec<u8>>>()?,
        };
        CString::new(bytes).ok()
    }

    /// A word from Hunspell, or None if it isn't valid in this encoding.
    fn decode(self, bytes: &[u8]) -> Option<String> {
        match self {
            Encoding::Utf8 => String::from_utf8(bytes.to_vec()).ok(),
            Encoding::Latin1 => Some(bytes.iter().map(|&b| char::from(b)).collect()),
        }
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Latin1 => "ISO8859-1",
        })
    }
}

/// Why a dictionary couldn't be opened.
#[derive(Debug)]
pub enum Error {
    /// The file doesn't exist.
    Missing(PathBuf),
    /// The file exists but couldn't be read (permissions, a directory, an
    /// I/O error).
    Unreadable(PathBuf, io::Error),
    /// The file is larger than [`MAX_FILE_BYTES`].
    TooLarge { path: PathBuf, bytes: u64 },
    /// The path can't be passed to Hunspell (it has a NUL byte).
    BadPath(PathBuf),
    /// The `.aff` names an encoding other than UTF-8 or ISO8859-1.
    UnsupportedEncoding(String),
    /// The `.dic` doesn't start with a word count.
    BadDic(PathBuf),
    /// Hunspell failed while loading (out of memory, for example).
    LoadFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Missing(p) => write!(f, "{} is missing", p.display()),
            Error::Unreadable(p, e) => write!(f, "{} can't be read: {e}", p.display()),
            Error::TooLarge { path, bytes } => {
                write!(f, "{} is too large ({bytes} bytes, at most {MAX_FILE_BYTES})", path.display())
            }
            Error::BadPath(p) => write!(f, "{} has a NUL byte in its path", p.display()),
            Error::UnsupportedEncoding(name) => write!(f, "unsupported encoding {name}"),
            Error::BadDic(p) => write!(f, "{} doesn't start with a word count", p.display()),
            Error::LoadFailed => f.write_str("Hunspell failed to load the dictionary"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Unreadable(_, e) => Some(e),
            _ => None,
        }
    }
}

/// An open Hunspell dictionary.
///
/// `Send` but not `Sync`: Hunspell keeps no global state, so a dictionary
/// can move to the thread that uses it, but its lookups aren't safe to run
/// from two threads at once.
pub struct Hunspell {
    handle: NonNull<ffi::Hunhandle>,
    encoding: Encoding,
    _not_sync: PhantomData<std::cell::Cell<()>>,
}

// SAFETY: a Hunhandle owns all its state and Hunspell 1.7 has no mutable
// globals, so using it from another thread than the one that created it is
// fine. `Cell` above keeps it from being `Sync`.
unsafe impl Send for Hunspell {}

impl Hunspell {
    /// Opens the dictionary in `aff` and `dic` (a LibreOffice-style pair),
    /// after checking both files. Loading parses the whole `.dic`: it takes
    /// tens of milliseconds on a desktop for the main Western languages, so
    /// call it off the UI thread.
    pub fn open(aff: &Path, dic: &Path) -> Result<Hunspell, Error> {
        check_file(aff)?;
        check_file(dic)?;
        let name = read_encoding(aff)?;
        let encoding = Encoding::from_name(&name).ok_or(Error::UnsupportedEncoding(name))?;
        check_word_count(dic)?;

        let aff_c = CString::new(aff.as_os_str().as_bytes()).map_err(|_| Error::BadPath(aff.to_path_buf()))?;
        let dic_c = CString::new(dic.as_os_str().as_bytes()).map_err(|_| Error::BadPath(dic.to_path_buf()))?;
        // SAFETY: two valid NUL-terminated paths; the shim catches exceptions.
        let handle = unsafe { ffi::verso_hunspell_create(aff_c.as_ptr(), dic_c.as_ptr()) };
        let handle = NonNull::new(handle).ok_or(Error::LoadFailed)?;
        let dict = Hunspell { handle, encoding, _not_sync: PhantomData };
        // The .aff was read above, but trust what Hunspell parsed.
        // SAFETY: the handle is valid; the string is owned by it.
        let parsed = unsafe { CStr::from_ptr(ffi::Hunspell_get_dic_encoding(dict.handle.as_ptr())) };
        let parsed = parsed.to_string_lossy();
        if Encoding::from_name(&parsed) != Some(encoding) {
            return Err(Error::UnsupportedEncoding(parsed.into_owned()));
        }
        Ok(dict)
    }

    /// The dictionary's encoding.
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// The base forms Hunspell finds for `word`, in its order (the word
    /// itself when it is already a base form). Empty when the word is
    /// unknown, empty, has a character the dictionary's encoding can't hold,
    /// or Hunspell fails.
    pub fn stem(&self, word: &str) -> Vec<String> {
        let Some(word) = self.encoding.encode(word) else { return Vec::new() };
        if word.as_bytes().is_empty() {
            return Vec::new();
        }
        let mut list: *mut *mut c_char = std::ptr::null_mut();
        // SAFETY: a valid handle and NUL-terminated word; Hunspell allocates
        // `list` with `n` entries, freed below with Hunspell_free_list.
        let n: c_int = unsafe { ffi::verso_hunspell_stem(self.handle.as_ptr(), &mut list, word.as_ptr()) };
        if n <= 0 || list.is_null() {
            return Vec::new();
        }
        let stems = (0..n as usize)
            .filter_map(|i| {
                // SAFETY: `list` has `n` entries, each NULL or a C string.
                let p = unsafe { *list.add(i) };
                if p.is_null() {
                    return None;
                }
                self.encoding.decode(unsafe { CStr::from_ptr(p) }.to_bytes())
            })
            .collect();
        // SAFETY: the list and count Hunspell_stem returned.
        unsafe { ffi::Hunspell_free_list(self.handle.as_ptr(), &mut list, n) };
        stems
    }
}

impl Drop for Hunspell {
    fn drop(&mut self) {
        // SAFETY: the handle came from Hunspell_create and is destroyed once.
        unsafe { ffi::Hunspell_destroy(self.handle.as_ptr()) }
    }
}

/// The file exists, is a regular readable file and isn't too large.
fn check_file(path: &Path) -> Result<(), Error> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::Missing(path.to_path_buf())),
        Err(e) => return Err(Error::Unreadable(path.to_path_buf(), e)),
    };
    if !meta.is_file() {
        return Err(Error::Unreadable(path.to_path_buf(), io::Error::other("not a regular file")));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge { path: path.to_path_buf(), bytes: meta.len() });
    }
    File::open(path).map_err(|e| Error::Unreadable(path.to_path_buf(), e))?;
    Ok(())
}

/// The `SET` name in an `.aff`, or Hunspell's default `ISO8859-1`.
fn read_encoding(aff: &Path) -> Result<String, Error> {
    let file = File::open(aff).map_err(|e| Error::Unreadable(aff.to_path_buf(), e))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).map_err(|e| Error::Unreadable(aff.to_path_buf(), e))?;
        if read == 0 {
            return Ok("ISO8859-1".to_string());
        }
        let text = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&line);
        let mut parts = text.split(|b| b.is_ascii_whitespace()).filter(|p| !p.is_empty());
        if parts.next() == Some(b"SET") {
            let name = parts.next().unwrap_or_default();
            return Ok(String::from_utf8_lossy(name).into_owned());
        }
    }
}

/// The `.dic` starts with a word count Hunspell accepts: an optional BOM,
/// optional spaces, then a number from 1 to just under 10 million.
fn check_word_count(dic: &Path) -> Result<(), Error> {
    let file = File::open(dic).map_err(|e| Error::Unreadable(dic.to_path_buf(), e))?;
    let mut first = Vec::new();
    BufReader::new(file.take(256))
        .read_until(b'\n', &mut first)
        .map_err(|e| Error::Unreadable(dic.to_path_buf(), e))?;
    let text = first.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&first);
    let digits: Vec<u8> =
        text.iter().copied().skip_while(|b| b.is_ascii_whitespace()).take_while(u8::is_ascii_digit).collect();
    let count = std::str::from_utf8(&digits).ok().and_then(|d| d.parse::<u64>().ok());
    match count {
        Some(n) if (1..MAX_WORD_COUNT).contains(&n) => Ok(()),
        _ => Err(Error::BadDic(dic.to_path_buf())),
    }
}
