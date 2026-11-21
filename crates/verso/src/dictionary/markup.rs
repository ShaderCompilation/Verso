//! A forgiving, streaming tag reader. No DOM, CSS engine or external resources.
use super::{Block, BlockKind, Span, Style, text::entities};

/// Elements that never have content or a closing tag.
fn void(name: &str) -> bool {
    matches!(
        name,
        "br" | "hr"
            | "img"
            | "input"
            | "meta"
            | "link"
            | "wbr"
            | "source"
            | "embed"
            | "param"
            | "track"
            | "area"
            | "col"
            | "base"
            | "keygen"
    )
}
/// IPA letters Literata lacks; a word containing one is set as phonetic.
fn ipa(c: char) -> bool {
    matches!(c, '\u{0250}'..='\u{02af}' | 'ː' | 'ˈ' | 'ˌ')
}
fn black(value: &str) -> bool {
    let value: String = value.trim_end_matches("!important").chars().filter(|c| !c.is_whitespace()).collect();
    matches!(
        value.as_str(),
        "" | "black"
            | "#000"
            | "#000f"
            | "#000000"
            | "#000000ff"
            | "rgb(0,0,0)"
            | "inherit"
            | "initial"
            | "unset"
            | "currentcolor"
            | "windowtext"
    )
}

struct Tag<'a> {
    name: String,
    attrs: Vec<(String, String)>,
    closing: bool,
    self_closing: bool,
    rest: &'a str,
}
impl<'a> Tag<'a> {
    fn read(source: &'a str) -> Option<Self> {
        let token = source.strip_prefix('<')?;
        let mut quote = None;
        let end = token.char_indices().take_while(|(i, _)| *i < 16384).find_map(|(i, c)| {
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
            } else if matches!(c, '\'' | '"') {
                quote = Some(c);
            } else if c == '<' {
                return Some((i, false));
            } else if c == '>' {
                return Some((i, true));
            }
            None
        })?;
        if !end.1 {
            return None;
        }
        let mut body = token[..end.0].trim_end();
        let closing = body.starts_with('/');
        body = body.strip_prefix('/').unwrap_or(body);
        let n = body.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(body.len());
        if n == 0 || !body.as_bytes()[0].is_ascii_alphabetic() {
            return None;
        }
        let name = body[..n].to_ascii_lowercase();
        let self_closing = body.ends_with('/');
        let mut attrs = Vec::new();
        body = &body[n..];
        while !body.trim_start().is_empty() {
            body = body.trim_start();
            let n = body.find(|c: char| c.is_whitespace() || c == '=' || c == '/').unwrap_or(body.len());
            if n == 0 {
                break;
            }
            let key = body[..n].to_ascii_lowercase();
            body = body[n..].trim_start();
            let mut value = "";
            if let Some(rest) = body.strip_prefix('=') {
                body = rest.trim_start();
                if body.starts_with(['\'', '"']) {
                    let q = body.as_bytes()[0] as char;
                    body = &body[1..];
                    let n = body.find(q).unwrap_or(body.len());
                    value = &body[..n];
                    body = body.get(n + 1..).unwrap_or("");
                } else {
                    let n = body.find(char::is_whitespace).unwrap_or(body.len());
                    value = body[..n].trim_end_matches('/');
                    body = &body[n..];
                }
            }
            attrs.push((key, entities(value)));
        }
        Some(Self { name, attrs, closing, self_closing, rest: &token[end.0 + 1..] })
    }
    fn empty(&self, kind: u8) -> bool {
        // HTML and Pango-style fields tolerate a stray slash on non-void
        // tags. XDXF is XML, where the slash really closes an empty element.
        void(&self.name) || kind == b'x' && self.self_closing
    }
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    fn audio(&self) -> bool {
        self.name == "rref"
            || self.name == "a" && self.attr("href").is_some_and(|v| v.to_ascii_lowercase().starts_with("sound://"))
    }
    fn hidden(&self) -> bool {
        self.audio() || matches!(self.name.as_str(), "script" | "style" | "object" | "audio" | "video" | "iframe")
    }
    fn style(&self, kind: u8) -> Style {
        let mut s = match self.name.as_str() {
            "b" | "strong" => Style::BOLD,
            "i" | "em" | "cite" | "var" | "ex" => Style::ITALIC,
            "c" | "abr" | "co" | "small" => Style::MUTED,
            "kref" | "iref" => Style::LINK,
            "a" if self.attr("href").is_some() => Style::LINK,
            "tr" if kind == b'x' => Style::PHONETIC,
            _ => Style::default(),
        };
        let mut apply = |key: &str, value: &str| {
            let value = value.trim().to_ascii_lowercase();
            match key.trim() {
                "font-weight" | "weight" | "font_weight"
                    if matches!(value.as_str(), "bold" | "semibold" | "heavy" | "ultrabold")
                        || value.parse::<u16>().is_ok_and(|v| v >= 600) =>
                {
                    s = s.union(Style::BOLD)
                }
                "font-style" | "style" | "font_style" if value == "italic" || value == "oblique" => {
                    s = s.union(Style::ITALIC)
                }
                // Pango font descriptions, such as `font="Serif Bold Italic 12"`.
                "font" | "font_desc" => {
                    for word in value.split_whitespace() {
                        match word {
                            "bold" | "semi-bold" | "semibold" | "heavy" | "ultra-bold" => s = s.union(Style::BOLD),
                            "italic" | "oblique" => s = s.union(Style::ITALIC),
                            _ => (),
                        }
                    }
                }
                "color" | "foreground" | "fgcolor" if !black(&value) => s = s.union(Style::MUTED),
                _ => (),
            }
        };
        for (key, value) in &self.attrs {
            if key == "style" && value.contains(':') {
                for declaration in value.split(';') {
                    if let Some((k, v)) = declaration.split_once(':') {
                        apply(&k.trim().to_ascii_lowercase(), v);
                    }
                }
            } else {
                apply(key, value);
            }
        }
        s
    }
    fn boundary(&self, kind: u8) -> bool {
        matches!(
            self.name.as_str(),
            "p" | "div"
                | "blockquote"
                | "def"
                | "dd"
                | "dt"
                | "li"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "ul"
                | "ol"
                | "hr"
        ) || self.name == "tr" && kind != b'x'
            || self.name == "k" && kind == b'x'
    }
    fn indent(&self) -> bool {
        matches!(self.name.as_str(), "blockquote" | "dd" | "ul" | "ol" | "li")
    }
    fn heading(&self) -> bool {
        matches!(self.name.as_str(), "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
    }
}
struct Frame {
    name: String,
    style: Style,
    depth: u8,
    heading: bool,
    boundary: bool,
    content: usize,
    script: Option<(usize, usize, bool)>,
}
/// HTML end tags that may be left out: an open `li`, `dd`/`dt` or `p` that a
/// new element closes. Returns the stack index to close at.
fn implied_end(stack: &[Frame], name: &str) -> Option<usize> {
    let targets: &[&str] = match name {
        "li" => &["li"],
        "dd" | "dt" => &["dd", "dt"],
        "p" | "div" | "blockquote" | "ul" | "ol" | "dl" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "hr" | "table"
        | "pre" => &["p"],
        _ => return None,
    };
    for (i, f) in stack.iter().enumerate().rev() {
        if targets.contains(&f.name.as_str()) {
            return Some(i);
        }
        // A list item may contain paragraphs and divs; a paragraph contains
        // only inline elements. Anything else ends the search.
        let passable = !f.boundary || targets[0] != "p" && matches!(f.name.as_str(), "p" | "div");
        if !passable || matches!(f.name.as_str(), "ul" | "ol" | "dl" | "table" | "td" | "th") {
            return None;
        }
    }
    None
}
#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    text: String,
    spans: Vec<Span>,
    space: bool,
    newline: bool,
    depth: u8,
    heading: bool,
    content: usize,
}
impl Builder {
    fn flush(&mut self) -> bool {
        self.space = false;
        if self.text.is_empty() {
            return false;
        }
        self.blocks.push(Block {
            kind: if self.heading { BlockKind::Heading } else { BlockKind::Text },
            depth: self.depth,
            text: std::mem::take(&mut self.text),
            spans: std::mem::take(&mut self.spans),
        });
        true
    }
    fn special(&mut self, kind: BlockKind) {
        self.flush();
        if kind != BlockKind::Gap || self.blocks.last().is_some_and(|b| b.kind != BlockKind::Gap) {
            self.blocks.push(Block { kind, depth: self.depth, ..Block::plain("") });
        }
    }
    fn push(&mut self, ch: char, style: Style) {
        let start = self.text.len() as u32;
        self.text.push(ch);
        let end = self.text.len() as u32;
        if let Some(last) = self.spans.last_mut()
            && last.end == start
            && last.style == style
        {
            last.end = end;
        } else {
            self.spans.push(Span { start, end, style });
        }
    }
    fn append(&mut self, text: &str, style: Style, lines: bool) {
        let normalized;
        let mut rest = if text.contains('\r') {
            normalized = text.replace("\r\n", "\n").replace('\r', "\n");
            normalized.as_str()
        } else {
            text
        };
        while let Some(c) = rest.chars().next() {
            if c.is_whitespace() {
                if c == '\n' && lines {
                    if self.newline {
                        self.special(BlockKind::Gap);
                    } else {
                        self.flush();
                    }
                    self.newline = true;
                } else {
                    self.space = !self.text.is_empty();
                }
                rest = &rest[c.len_utf8()..];
                continue;
            }
            let n = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let (word, after) = rest.split_at(n);
            rest = after;
            // Phonetic per word, so one transcription doesn't switch a whole
            // paragraph or plain-text field to the phonetic face.
            let word_style = if word.chars().any(ipa) { style.union(Style::PHONETIC) } else { style };
            for c in word.chars().filter(|c| !c.is_control()) {
                self.newline = false;
                if self.space {
                    // A space between differently styled runs keeps only
                    // their shared styles: no underline before a link.
                    let previous = self.spans.last().map_or(word_style, |s| s.style);
                    self.push(' ', previous.intersection(word_style));
                    self.space = false;
                }
                self.push(if c == '》' { ')' } else { c }, word_style);
                self.content += 1;
            }
        }
    }
    /// Close `stack[i]` and everything opened inside it.
    fn close(&mut self, stack: &mut Vec<Frame>, i: usize) {
        if stack[i..].iter().any(|f| f.boundary) {
            self.flush();
            if stack[i].content == self.content {
                self.special(BlockKind::Gap);
            }
        }
        stack.truncate(i);
        self.depth = stack.last().map_or(0, |f| f.depth);
        self.heading = stack.last().is_some_and(|f| f.heading);
    }
}
// Look ahead only within this element, to hide it, discard an audio label or
// convert a simple superscript. Nested markup is otherwise handled by the
// normal stack. Scans stop at a 33rd nested opener, which keeps repeated
// unclosed elements from rescanning a whole field each time.
fn element_end<'a>(source: &'a str, name: &str, kind: u8) -> Option<(&'a str, &'a str)> {
    // The scan can't be cancelled, so bound it: an element longer than this is
    // treated as unclosed (only its tag is dropped).
    const SCAN_MAX: usize = 64 * 1024;
    let mut cut = SCAN_MAX.min(source.len());
    while !source.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut rest = &source[..cut];
    let mut depth = 1usize;
    while let Some(i) = rest.find('<') {
        let at = &rest[i..];
        if let Some(tag) = Tag::read(at) {
            if tag.name == name {
                if tag.closing {
                    depth -= 1;
                } else if !tag.empty(kind) {
                    depth += 1;
                    if depth > 32 {
                        return None;
                    }
                }
                if depth == 0 {
                    return Some((&source[..cut - at.len()], &source[cut - tag.rest.len()..]));
                }
            }
            rest = tag.rest;
        } else {
            rest = &at[1..];
        }
    }
    None
}
/// Digits, signs and parentheses only: these exist in Literata (signs fall
/// back to Inter). Letters such as `st` in `1<sup>st</sup>` stay as text.
fn script(text: &str, sub: bool) -> Option<String> {
    let to = if sub {
        "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎"
    } else {
        "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾"
    };
    text.chars()
        .map(|c| {
            if c.is_whitespace() {
                Some(c)
            } else {
                "0123456789+-=()".chars().position(|a| a == c).and_then(|i| to.chars().nth(i))
            }
        })
        .collect()
}
#[cfg(test)]
fn parse(source: &str, kind: u8) -> Vec<Block> {
    let mut blocks = parse_field(source, kind, &mut true);
    normalize(&mut blocks);
    blocks
}
/// `first_k` is true until an entry shows content; only a `<k>` before any
/// content is the index headword and dropped.
pub(super) fn parse_field(source: &str, kind: u8, first_k: &mut bool) -> Vec<Block> {
    let mut out = Builder::default();
    let mut stack: Vec<Frame> = Vec::new();
    let markup = matches!(kind, b'h' | b'x' | b'g');
    // HTML collapses newlines; XDXF, Pango and plain text break lines on them.
    let lines = kind != b'h';
    let mut rest = source;
    while !rest.is_empty() {
        if markup && rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |i| &rest[i + 3..]);
            continue;
        }
        if markup && let Some(body) = rest.strip_prefix("<![CDATA[") {
            let n = body.find("]]>").unwrap_or(body.len());
            out.append(&body[..n], stack.last().map_or(Style::default(), |f| f.style), lines);
            rest = body.get(n + 3..).unwrap_or("");
            continue;
        }
        // `<!DOCTYPE …>` and `<?xml …?>` declarations.
        if markup
            && (rest.starts_with("<!") || rest.starts_with("<?"))
            && rest[2..].starts_with(|c: char| c.is_ascii_alphabetic())
            && let Some(end) = rest.bytes().take(16384).position(|b| b == b'>')
        {
            rest = &rest[end + 1..];
            continue;
        }
        if markup && let Some(tag) = Tag::read(rest) {
            rest = tag.rest;
            if !tag.closing {
                if matches!(tag.name.as_str(), "abr" | "c")
                    && let Some((_, after)) = element_end(rest, &tag.name, kind)
                    && Tag::read(after.trim_start()).is_some_and(|t| !t.closing && t.audio())
                {
                    rest = after;
                    continue;
                }
                let headword = tag.name == "k" && kind == b'x' && *first_k && out.content == 0;
                if (tag.hidden() || headword) && !tag.empty(kind) {
                    // Hide only a closed element: an unclosed one would
                    // swallow the rest of the field, so drop just its tag.
                    if let Some((_, after)) = element_end(rest, &tag.name, kind) {
                        *first_k &= !headword;
                        rest = after;
                        continue;
                    }
                }
                if tag.hidden() {
                    continue;
                }
                if kind == b'h'
                    && let Some(i) = implied_end(&stack, &tag.name)
                {
                    out.close(&mut stack, i);
                }
                if tag.name == "br" {
                    if !out.flush() {
                        out.special(BlockKind::Gap);
                    }
                    continue;
                }
                if tag.name == "hr" {
                    out.special(BlockKind::Rule);
                    continue;
                }
                if tag.boundary(kind) {
                    out.flush();
                }
                if tag.empty(kind) {
                    if tag.boundary(kind) {
                        out.special(BlockKind::Gap);
                    }
                    continue;
                }
                if stack.len() < 32 {
                    let style = stack.last().map_or(Style::default(), |f| f.style).union(tag.style(kind));
                    let depth = out.depth + u8::from(tag.indent());
                    let heading = out.heading || tag.heading();
                    stack.push(Frame {
                        name: tag.name.clone(),
                        style,
                        depth,
                        heading,
                        boundary: tag.boundary(kind),
                        content: out.content,
                        script: matches!(tag.name.as_str(), "sup" | "sub").then_some((
                            out.text.len(),
                            out.blocks.len(),
                            tag.name == "sub",
                        )),
                    });
                    out.depth = depth;
                    out.heading = heading;
                }
            } else if let Some(i) = stack.iter().rposition(|f| f.name == tag.name) {
                if let Some((start, block, sub)) = stack[i].script
                    && block == out.blocks.len()
                    && let Some(converted) = script(&out.text[start..], sub)
                {
                    let old = out.text[start..].to_owned();
                    let i = out.spans.partition_point(|s| s.end <= start as u32);
                    let spans = out.spans.split_off(i);
                    if let Some(first) = spans.first()
                        && first.start < start as u32
                    {
                        out.spans.push(Span { end: start as u32, ..first.clone() });
                    }
                    out.text.truncate(start);
                    let mut offset = start as u32;
                    let mut spans = spans.iter().peekable();
                    for (before, after) in old.chars().zip(converted.chars()) {
                        while spans.peek().is_some_and(|s| s.end <= offset) {
                            spans.next();
                        }
                        out.push(after, spans.peek().unwrap().style);
                        offset += before.len_utf8() as u32;
                    }
                }
                out.close(&mut stack, i);
            }
            if tag.closing && matches!(tag.name.as_str(), "td" | "th") {
                out.space = !out.text.is_empty();
            }
            continue;
        }
        let first = rest.chars().next().unwrap().len_utf8();
        let n = if markup { rest[first..].find('<').map_or(rest.len(), |i| i + first) } else { rest.len() };
        out.append(&entities(&rest[..n]), stack.last().map_or(Style::default(), |f| f.style), lines);
        rest = &rest[n..];
    }
    out.flush();
    while out.blocks.last().is_some_and(|b| b.kind == BlockKind::Gap) {
        out.blocks.pop();
    }
    *first_k &= out.content == 0;
    out.blocks
}
pub(super) fn normalize(blocks: &mut [Block]) {
    let min = blocks.iter().filter(|b| !b.text.is_empty()).map(|b| b.depth).min().unwrap_or(0);
    for b in blocks {
        b.depth = b.depth.saturating_sub(min).min(4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Original fixtures patterned after dictionary structures, never excerpts.
    const XDXF: &str = "<ar><k>sample</k><blockquote><b>sample<sup>1</sup></b><abr>noun</abr><blockquote><c c='blue'>1》</c> A <b>bold <i>nested</i></b> sense.<blockquote><ex>A <b>strong</b> example.</ex></blockquote><kref>other</kref></blockquote><tr>ˈsɑːmpəl</tr><abr>UK</abr> <rref>uk.wav</rref> <c c='red'>US</c> <a href='sound://us.wav'>listen</a><abr>rare</abr><k>later</k></blockquote></ar>";
    const HTML: &str = "<div><h4>Origins</h4><blockquote><span style='font-weight: 600; font-style: italic; color: #a00'>A styled quotation</span><br><font color='blue'>label</font> 10<sup>-18</sup> H<sub>2</sub>O <sup>Q?</sup><a href='entry'>reference</a></blockquote><hr><table><tr><td>one</td><td>two</td></tr></table></div>";
    const PANGO: &str = "<span weight='bold' style='italic' foreground='red'>Жёлтый</span> <span color='black'>black</span> <strong>firm</strong> <em>soft</em> <co>note</co> <small>small</small>";
    #[test]
    fn element_scan_is_bounded() {
        let far = format!("<script>{}</script>tail", "x".repeat(70 * 1024));
        let text = lines(&parse(&far, b'h'));
        assert!(text.contains("tail"));
        let near = lines(&parse("a<script>hidden</script>b", b'h'));
        assert_eq!(near, "ab");
    }
    fn lines(blocks: &[Block]) -> String {
        blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n")
    }
    fn style_at(blocks: &[Block], text: &str) -> Style {
        let b = blocks.iter().find(|b| b.text.contains(text)).unwrap();
        let i = b.text.find(text).unwrap() as u32;
        b.spans.iter().find(|s| s.start <= i && i < s.end).unwrap().style
    }
    fn depths(blocks: &[Block]) -> Vec<u8> {
        blocks.iter().map(|b| b.depth).collect()
    }
    fn validate(blocks: &[Block]) {
        for b in blocks {
            let mut end = 0;
            for s in &b.spans {
                assert_eq!(s.start, end);
                assert!(s.start < s.end && s.end <= b.text.len() as u32);
                assert!(b.text.is_char_boundary(s.start as usize) && b.text.is_char_boundary(s.end as usize));
                end = s.end;
            }
            assert_eq!(end as usize, b.text.len());
        }
    }
    #[test]
    fn styles_structure_and_audio() {
        let b = parse(XDXF, b'x');
        validate(&b);
        assert_eq!(lines(&b), "sample¹noun\n1) A bold nested sense.\nA strong example.\nother\nˈsɑːmpəl rare\nlater");
        assert_eq!(depths(&b), [0, 1, 2, 1, 0, 0]);
        for (text, style) in [
            ("sample", Style::BOLD),
            ("noun", Style::MUTED),
            ("1)", Style::MUTED),
            ("nested", Style::BOLD.union(Style::ITALIC)),
            ("strong", Style::BOLD.union(Style::ITALIC)),
            ("other", Style::LINK),
            ("ˈsɑːmpəl", Style::PHONETIC),
            ("rare", Style::MUTED),
        ] {
            assert!(style_at(&b, text).contains(style), "{text}");
        }
        assert_eq!(
            lines(&parse(
                "<abr>UK</abr> <rref><rref>nested</rref>audio</rref> <abr>US</abr><a HREF='sound://a'>audio</a> <abr>kept</abr> text",
                b'x'
            )),
            "kept text"
        );
        assert_eq!(lines(&parse("<k>first</k><k>second</k>", b'x')), "second");
        assert_eq!(lines(&parse("<k>html headword</k>", b'h')), "html headword");
        // A `<k>` after content is a later headword, kept on its own line.
        assert_eq!(lines(&parse("<blockquote>intro</blockquote><k>real</k>text", b'x')), "intro\nreal\ntext");
        assert_eq!(lines(&parse("<k/><k>first</k>text", b'x')), "text");
        // Only a link with a target is styled as one.
        let b = parse("<a name='x'>anchor</a> <a href='y'>link</a>", b'h');
        assert_eq!(style_at(&b, "anchor"), Style::default());
        assert_eq!(style_at(&b, "link"), Style::LINK);
        // Collapsed spaces between runs keep only the shared styles, so a
        // link's underline starts at its first letter.
        let b = parse("such as a  <kref>lamp</kref>  or <b>firm <i>soft</i></b>", b'x');
        validate(&b);
        assert_eq!(lines(&b), "such as a lamp or firm soft");
        assert_eq!(style_at(&b, " lamp"), Style::default());
        assert_eq!(style_at(&b, " or"), Style::default());
        assert_eq!(style_at(&b, " soft"), Style::BOLD);
    }
    #[test]
    fn html_pango_scripts_and_entities() {
        let b = parse(HTML, b'h');
        validate(&b);
        assert_eq!(lines(&b), "Origins\nA styled quotation\nlabel 10⁻¹⁸ H₂O Q?reference\n\none two");
        assert_eq!(b[0].kind, BlockKind::Heading);
        assert_eq!(b[3].kind, BlockKind::Rule);
        assert!(style_at(&b, "quotation").contains(Style::BOLD.union(Style::ITALIC).union(Style::MUTED)));
        assert!(style_at(&b, "label").contains(Style::MUTED));
        assert!(style_at(&b, "reference").contains(Style::LINK));
        let b = parse(PANGO, b'g');
        validate(&b);
        assert!(style_at(&b, "Жёлтый").contains(Style::BOLD.union(Style::ITALIC).union(Style::MUTED)));
        assert!(!style_at(&b, "black").contains(Style::MUTED));
        let b = parse(
            "<span fgcolor='#333' font_weight='bold'>a</span> <span font_style='italic'>b</span> <span font='Serif Bold Italic 12'>c</span> <span color='inherit'>d</span> <span style='color: rgb(0, 0, 0) !important'>e</span>",
            b'g',
        );
        assert_eq!(style_at(&b, "a"), Style::MUTED.union(Style::BOLD));
        assert_eq!(style_at(&b, "b"), Style::ITALIC);
        assert_eq!(style_at(&b, "c"), Style::BOLD.union(Style::ITALIC));
        assert_eq!(style_at(&b, "d"), Style::default());
        assert_eq!(style_at(&b, "e"), Style::default());
        assert_eq!(
            lines(&parse("a<sup><b>1</b></sup> 10<sup>-18</sup> <sup>Q?</sup> 1<sup>st</sup> x<sub>i</sub>", b'h')),
            "a¹ 10⁻¹⁸ Q? 1st xi"
        );
        assert_eq!(lines(&parse("&NotEqualTilde; &#x416; &unknown; &lt;b&gt;", b'h')), "≂̸ Ж &unknown; <b>");
    }
    #[test]
    fn stray_slash_opens_non_void_styles_except_in_xdxf() {
        let html = parse("<i/>word</i> plain <b/>bold", b'h');
        validate(&html);
        assert_eq!(style_at(&html, "word"), Style::ITALIC);
        assert_eq!(style_at(&html, "plain"), Style::default());
        assert_eq!(style_at(&html, "bold"), Style::BOLD);

        let pango = parse("<span style='font-style: italic'/>word</span> plain", b'g');
        validate(&pango);
        assert_eq!(style_at(&pango, "word"), Style::ITALIC);
        assert_eq!(style_at(&pango, "plain"), Style::default());

        let xdxf = parse("<i/>plain <i>styled</i>", b'x');
        validate(&xdxf);
        assert_eq!(style_at(&xdxf, "plain"), Style::default());
        assert_eq!(style_at(&xdxf, "styled"), Style::ITALIC);
    }
    #[test]
    fn whitespace_gaps_and_inert_resources() {
        assert_eq!(
            lines(&parse("\n<p></p><div></div><p>one</p><blockquote></blockquote><div></div><p>two</p><p></p>", b'h')),
            "one\n\ntwo"
        );
        assert_eq!(lines(&parse("one\n two\n\n\nthree\n", b'x')), "one\ntwo\n\nthree");
        assert_eq!(lines(&parse("one\n two\n\nthree", b'h')), "one two three");
        assert_eq!(lines(&parse("<span>one\ntwo</span>\n\nthree", b'g')), "one\ntwo\n\nthree");
        assert_eq!(lines(&parse("<def>one</def>\n<def>two</def>", b'x')), "one\ntwo");
        assert_eq!(lines(&parse("one\t two\r\n\r\nthree <b>literal</b>", b'm')), "one two\n\nthree <b>literal</b>");
        assert_eq!(lines(&parse("one<br>two<br><br>three", b'h')), "one\ntwo\n\nthree");
        assert_eq!(
            lines(&parse(
                "a<script>hidden<script>nested</script>hidden</script><style>bad</style><audio>bad</audio><video>bad</video><iframe>bad</iframe><object>bad</object><img src='ignored'>b",
                b'h'
            )),
            "ab"
        );
        assert_eq!(
            lines(&parse("<!DOCTYPE html><?xml version='1.0'?><![CDATA[a <b> c]]> <!x and <?", b'h')),
            "a <b> c <!x and <?"
        );
    }
    #[test]
    fn phonetic_style_marks_only_words_with_ipa() {
        let b = parse("line one\nsay [ˈset] twice\nline three", b'm');
        validate(&b);
        assert_eq!(style_at(&b, "line one"), Style::default());
        assert_eq!(style_at(&b, "say "), Style::default());
        assert_eq!(style_at(&b, "[ˈset]"), Style::PHONETIC);
        assert_eq!(style_at(&b, " twice"), Style::default());
        let b = parse("<i>ˈsɑː mpəl and</i>", b'h');
        assert_eq!(style_at(&b, "ˈsɑː mpəl"), Style::ITALIC.union(Style::PHONETIC));
        assert_eq!(style_at(&b, " and"), Style::ITALIC);
    }
    #[test]
    fn malformed_markup_keeps_text_and_bounds_stack() {
        for (source, expected) in [
            ("<b>one<i>two</b>three</i>", "onetwothree"),
            ("<b>one<i>two three", "onetwo three"),
            ("one < two <i>three", "one < two three"),
            ("one < two > three", "one < two > three"),
            ("one <broken", "one <broken"),
            ("<odd>one</odd>two</stray>three", "onetwothree"),
            // Unclosed hidden elements drop only their tag.
            ("before <rref>x.wav definition text", "before x.wav definition text"),
            ("<a href='sound://x'>UK</a> kept <a href='sound://y'>audio text", "kept audio text"),
            ("one <script>if (a < b) two", "one if (a < b) two"),
            ("<k>unclosed headword text", "unclosed headword text"),
        ] {
            let kind = if source.contains("rref") || source.contains("<k>") { b'x' } else { b'h' };
            let b = parse(source, kind);
            validate(&b);
            assert_eq!(lines(&b), expected, "{source}");
        }
        let source = format!("{}Ж{}tail", "<b>".repeat(40), "</b>".repeat(40));
        let b = parse(&source, b'x');
        validate(&b);
        assert_eq!(lines(&b), "Жtail");
        assert_eq!(style_at(&b, "tail"), Style::default());
        let b = parse("<blockquote><blockquote>x</blockquote>y</blockquote>z", b'x');
        assert_eq!(depths(&b), [2, 1, 0]);
    }
    #[test]
    fn html_implied_end_tags_and_void_elements() {
        let b = parse("<ul><li>one<li>two<p>para<li>three<ul><li>inner</ul></ul>after", b'h');
        assert_eq!(lines(&b), "one\ntwo\npara\nthree\ninner\nafter");
        assert_eq!(depths(&b), [2, 2, 2, 2, 4, 0]);
        let b = parse("<dl><dt>term<dd>meaning<dt>next<dd>more</dl>", b'h');
        assert_eq!(depths(&b), [0, 1, 0, 1]);
        // Unclosed paragraphs don't fill the stack and stop styling.
        let source = (0..40).map(|i| format!("<p>para{i}")).collect::<String>() + "<b>bold</b>";
        let b = parse(&source, b'h');
        assert_eq!(b.len(), 40);
        assert_eq!(style_at(&b, "bold"), Style::BOLD);
        let b = parse(&format!("{}<b>bold</b>", "<wbr><source src=a><embed><col>".repeat(10)), b'h');
        assert_eq!(style_at(&b, "bold"), Style::BOLD);
        // XDXF is XML: no implied end tags.
        assert_eq!(depths(&parse("<blockquote>a<blockquote>b</blockquote></blockquote>", b'x')), [0, 1]);
    }
    #[test]
    fn fixture_glyphs_are_covered_after_substitution() {
        use cosmic_text::skrifa::{FontRef, MetadataProvider};
        let covered = |data: &[u8], c: char| FontRef::new(data).unwrap().charmap().map(c).is_some();
        let inter = include_bytes!("../../../../assets/fonts/Inter-Regular.ttf").as_slice();
        let literata = include_bytes!("../../../../assets/fonts/Literata-Regular.ttf").as_slice();
        // Definitions are set in Literata; phonetic words and these signs,
        // which Literata lacks, come from Inter.
        let inter_only = "⁺⁻⁼₊₋₌";
        for (fixture, kind) in [(XDXF, b'x'), (HTML, b'h'), (PANGO, b'g')] {
            for b in parse(fixture, kind) {
                for s in &b.spans {
                    for c in b.text[s.start as usize..s.end as usize].chars().filter(|c| !c.is_whitespace()) {
                        let font =
                            if s.style.contains(Style::PHONETIC) || inter_only.contains(c) { inter } else { literata };
                        assert!(covered(font, c), "missing {c}");
                    }
                }
            }
        }
    }
}
