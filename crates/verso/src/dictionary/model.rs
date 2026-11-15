//! Dictionary content, independent of the screen and text renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub headword: String,
    pub blocks: Vec<Block>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub depth: u8,
    pub text: String,
    pub spans: Vec<Span>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Text,
    Heading,
    Gap,
    Rule,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub style: Style,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style(u8);
impl Style {
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(2);
    pub const MUTED: Self = Self(4);
    pub const PHONETIC: Self = Self(8);
    pub const LINK: Self = Self(16);
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}
impl Block {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { kind: BlockKind::Text, depth: 0, text: text.into(), spans: Vec::new() }
    }
    /// Keep a UTF-8 prefix and clip its style ranges with it.
    pub fn truncate(&mut self, limit: usize) {
        let mut end = limit.min(self.text.len());
        while !self.text.is_char_boundary(end) {
            end -= 1;
        }
        self.text.truncate(end);
        self.spans.retain_mut(|s| {
            s.end = s.end.min(end as u32);
            s.start < s.end
        });
    }
}
