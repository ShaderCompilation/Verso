//! Supported book file formats and extension matching.

use std::path::Path;

/// A book format Verso can index, inspect, and open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BookFormat {
    Epub,
    Markdown,
}

impl BookFormat {
    /// Classifies `path` by its file extension (case-insensitive).
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        Self::from_extension(ext)
    }

    /// Classifies a bare extension (without leading dot), case-insensitively.
    pub fn from_extension(ext: &str) -> Option<Self> {
        if ext.eq_ignore_ascii_case("epub") {
            Some(Self::Epub)
        } else if ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown") {
            Some(Self::Markdown)
        } else {
            None
        }
    }

    /// True if `path` has a supported book extension.
    pub fn is_book(path: &Path) -> bool {
        Self::from_path(path).is_some()
    }

    /// Canonical lowercase extension (without leading dot).
    pub fn extension(self) -> &'static str {
        match self {
            Self::Epub => "epub",
            Self::Markdown => "md",
        }
    }

    /// All recognized extensions (lowercase, without leading dot).
    pub const ALL_EXTENSIONS: [&'static str; 3] = ["epub", "md", "markdown"];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_supported_extensions_case_insensitively() {
        for (path, expected) in [
            ("book.epub", Some(BookFormat::Epub)),
            ("Book.EPUB", Some(BookFormat::Epub)),
            ("notes.md", Some(BookFormat::Markdown)),
            ("README.MD", Some(BookFormat::Markdown)),
            ("essay.markdown", Some(BookFormat::Markdown)),
            ("Essay.MarkDown", Some(BookFormat::Markdown)),
            ("notes.txt", None),
            ("no_extension", None),
            ("dir.epub/readme.txt", None),
        ] {
            let path = Path::new(path);
            assert_eq!(BookFormat::from_path(path), expected, "{}", path.display());
            assert_eq!(BookFormat::is_book(path), expected.is_some());
        }
        assert_eq!(BookFormat::Epub.extension(), "epub");
        assert_eq!(BookFormat::Markdown.extension(), "md");
        assert_eq!(BookFormat::ALL_EXTENSIONS, ["epub", "md", "markdown"]);
    }
}
