//! A forgiving, streaming tag reader. No DOM, CSS engine or external resources.
use super::{Block, BlockKind, Span, Style, text::entities};

struct Tag<'a> {
    name: String,
    attrs: Vec<(String, String)>,
    closing: bool,
    empty: bool,
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
        let empty = body.ends_with('/') || matches!(name.as_str(), "br" | "hr" | "img" | "input" | "meta" | "link");
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
        Some(Self { name, attrs, closing, empty, rest: &token[end.0 + 1..] })
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
            "kref" | "iref" | "a" => Style::LINK,
            "tr" if kind == b'x' => Style::PHONETIC,
            _ => Style::default(),
        };
        let mut apply = |key: &str, value: &str| {
            let value = value.trim().to_ascii_lowercase();
            match key.trim() {
                "font-weight" | "weight"
                    if matches!(value.as_str(), "bold" | "semibold" | "heavy" | "ultrabold")
                        || value.parse::<u16>().is_ok_and(|v| v >= 600) =>
                {
                    s = s.union(Style::BOLD)
                }
                "font-style" | "style" if value == "italic" || value == "oblique" => s = s.union(Style::ITALIC),
                "color" | "foreground"
                    if !matches!(value.as_str(), "black" | "#000" | "#000000" | "rgb(0,0,0)" | "rgb(0, 0, 0)")
                        && !value.is_empty() =>
                {
                    s = s.union(Style::MUTED)
                }
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
    fn append(&mut self, text: &str, mut style: Style, lines: bool) {
        if text.chars().any(|c| matches!(c, '\u{0250}'..='\u{02af}' | 'ː' | 'ˈ' | 'ˌ')) {
            style = style.union(Style::PHONETIC);
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        for c in text.chars() {
            if c == '\n' && lines {
                if self.newline {
                    self.special(BlockKind::Gap);
                } else {
                    self.flush();
                }
                self.newline = true;
            } else if c.is_whitespace() {
                self.space = !self.text.is_empty();
            } else if !c.is_control() {
                self.newline = false;
                if self.space {
                    self.push(' ', style);
                    self.space = false;
                }
                self.push(if c == '》' { ')' } else { c }, style);
                self.content += 1;
            }
        }
    }
}
// Look ahead only within this element, to discard an audio label or convert a
// simple superscript. Nested markup is otherwise handled by the normal stack.
fn element_end<'a>(source: &'a str, name: &str) -> Option<(&'a str, &'a str)> {
    let mut rest = source;
    let mut depth = 1usize;
    while let Some(i) = rest.find('<') {
        let at = &rest[i..];
        if let Some(tag) = Tag::read(at) {
            if tag.name == name {
                if tag.closing {
                    depth -= 1;
                } else if !tag.empty {
                    depth += 1;
                    if depth > 32 {
                        return None;
                    }
                }
                if depth == 0 {
                    return Some((&source[..source.len() - at.len()], tag.rest));
                }
            }
            rest = tag.rest;
        } else {
            rest = &at[1..];
        }
    }
    None
}
fn script(text: &str, sub: bool) -> Option<String> {
    let from = if sub { "0123456789+-=()aehijklmnoprstuvx" } else { "0123456789+-=()abcdefghijklmnoprstuvwxyz" };
    let to = if sub {
        "₀₁₂₃₄₅₆₇₈₉₊₋₌₍₎ₐₑₕᵢⱼₖₗₘₙₒₚᵣₛₜᵤᵥₓ"
    } else {
        "⁰¹²³⁴⁵⁶⁷⁸⁹⁺⁻⁼⁽⁾ᵃᵇᶜᵈᵉᶠᵍʰⁱʲᵏˡᵐⁿᵒᵖʳˢᵗᵘᵛʷˣʸᶻ"
    };
    text.chars()
        .map(|c| {
            if c.is_whitespace() { Some(c) } else { from.chars().position(|a| a == c).and_then(|i| to.chars().nth(i)) }
        })
        .collect()
}
#[cfg(test)]
fn parse(source: &str, kind: u8) -> Vec<Block> {
    let mut blocks = parse_field(source, kind, &mut true);
    normalize(&mut blocks);
    blocks
}
pub(super) fn parse_field(source: &str, kind: u8, first_k: &mut bool) -> Vec<Block> {
    let mut out = Builder::default();
    let mut stack: Vec<Frame> = Vec::new();
    let mut hidden: Option<(String, usize)> = None;
    let markup = matches!(kind, b'h' | b'x' | b'g');
    let mut rest = source;
    while !rest.is_empty() {
        if markup && rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |i| &rest[i + 3..]);
            continue;
        }
        if markup && let Some(tag) = Tag::read(rest) {
            rest = tag.rest;
            if let Some((name, depth)) = hidden.as_mut() {
                if *name == tag.name {
                    if tag.closing {
                        *depth -= 1;
                    } else if !tag.empty {
                        *depth += 1;
                    }
                }
                if *depth == 0 {
                    hidden = None;
                }
                continue;
            }
            if !tag.closing {
                if matches!(tag.name.as_str(), "abr" | "c")
                    && let Some((_, after)) = element_end(rest, &tag.name)
                    && Tag::read(after.trim_start()).is_some_and(|t| !t.closing && t.audio())
                {
                    rest = after;
                    continue;
                }
                let drop_k =
                    tag.name == "k" && kind == b'x' && *first_k && (tag.empty || element_end(rest, "k").is_some());
                if drop_k {
                    *first_k = false;
                }
                if tag.hidden() || drop_k {
                    if !tag.empty {
                        hidden = Some((tag.name, 1));
                    }
                    continue;
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
                if tag.empty {
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
                if stack[i..].iter().any(|f| f.boundary) {
                    out.flush();
                    if stack[i].content == out.content {
                        out.special(BlockKind::Gap);
                    }
                }
                stack.truncate(i);
                out.depth = stack.last().map_or(0, |f| f.depth);
                out.heading = stack.last().is_some_and(|f| f.heading);
            }
            if tag.closing && matches!(tag.name.as_str(), "td" | "th") {
                out.space = !out.text.is_empty();
            }
            continue;
        }
        let first = rest.chars().next().unwrap().len_utf8();
        let n = if markup { rest[first..].find('<').map_or(rest.len(), |i| i + first) } else { rest.len() };
        if hidden.is_none() {
            out.append(
                &entities(&rest[..n]),
                stack.last().map_or(Style::default(), |f| f.style),
                kind != b'h' && kind != b'g',
            );
        }
        rest = &rest[n..];
    }
    out.flush();
    while out.blocks.last().is_some_and(|b| b.kind == BlockKind::Gap) {
        out.blocks.pop();
    }
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
    fn lines(blocks: &[Block]) -> String {
        blocks.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n")
    }
    fn style_at(blocks: &[Block], text: &str) -> Style {
        let b = blocks.iter().find(|b| b.text.contains(text)).unwrap();
        let i = b.text.find(text).unwrap() as u32;
        b.spans.iter().find(|s| s.start <= i && i < s.end).unwrap().style
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
        assert_eq!(lines(&b), "sample¹noun\n1) A bold nested sense.\nA strong example.\nother\nˈsɑːmpəl rarelater");
        assert_eq!(b.iter().map(|b| b.depth).collect::<Vec<_>>(), [0, 1, 2, 1, 0]);
        for (text, style) in [
            ("sample", Style::BOLD),
            ("noun", Style::MUTED),
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
    }
    #[test]
    fn html_pango_scripts_and_entities() {
        let b = parse(HTML, b'h');
        validate(&b);
        assert_eq!(b[0].kind, BlockKind::Heading);
        assert!(style_at(&b, "quotation").contains(Style::BOLD.union(Style::ITALIC).union(Style::MUTED)));
        assert!(style_at(&b, "reference").contains(Style::LINK));
        assert!(b.iter().any(|b| b.kind == BlockKind::Rule));
        assert!(lines(&b).contains("10⁻¹⁸ H₂O Q?reference"));
        assert!(lines(&b).ends_with("one two"));
        let b = parse(PANGO, b'g');
        validate(&b);
        assert!(style_at(&b, "Жёлтый").contains(Style::BOLD.union(Style::ITALIC).union(Style::MUTED)));
        assert!(!style_at(&b, "black").contains(Style::MUTED));
        assert_eq!(lines(&parse("a<sup><b>1</b></sup> 10<sup>-18</sup> <sup>Q?</sup>", b'h')), "a¹ 10⁻¹⁸ Q?");
        assert_eq!(lines(&parse("&NotEqualTilde; &#x416; &unknown; &lt;b&gt;", b'h')), "≂̸ Ж &unknown; <b>");
    }
    #[test]
    fn whitespace_gaps_and_inert_resources() {
        assert_eq!(
            lines(&parse("\n<p></p><div></div><p>one</p><blockquote></blockquote><div></div><p>two</p><p></p>", b'h')),
            "one\n\ntwo"
        );
        assert_eq!(lines(&parse("one\n two\n\n\nthree\n", b'x')), "one\ntwo\n\nthree");
        assert_eq!(lines(&parse("one\n two\n\nthree", b'h')), "one two three");
        assert_eq!(lines(&parse("<def>one</def>\n<def>two</def>", b'x')), "one\ntwo");
        assert_eq!(lines(&parse("one\t two\r\n\r\nthree <b>literal</b>", b'm')), "one two\n\nthree <b>literal</b>");
        assert_eq!(
            lines(&parse(
                "a<script>hidden<script>nested</script>hidden</script><style>bad</style><audio>bad</audio><video>bad</video><iframe>bad</iframe><object>bad</object><img src='ignored'>b",
                b'h'
            )),
            "ab"
        );
    }
    #[test]
    fn malformed_markup_keeps_text_and_bounds_stack() {
        for source in [
            "<b>one<i>two</b>three</i>",
            "<b>one<i>two three",
            "one < two <i>three",
            "one < two > three",
            "one <broken",
            "<odd>one</odd>two</stray>three",
        ] {
            let b = parse(source, b'h');
            validate(&b);
            let text = lines(&b);
            assert!(text.contains("one"), "{source}: {text}");
            assert!(text.contains("two") || text.contains("broken"), "{source}: {text}");
        }
        let source = format!("{}Ж{}tail", "<b>".repeat(40), "</b>".repeat(40));
        let b = parse(&source, b'x');
        validate(&b);
        assert_eq!(lines(&b), "Жtail");
        assert_eq!(style_at(&b, "tail"), Style::default());
        let b = parse("<blockquote><blockquote>x</blockquote>y</blockquote>z", b'x');
        assert_eq!(b.iter().map(|b| b.depth).collect::<Vec<_>>(), [2, 1, 0]);
    }
    #[test]
    fn fixture_glyphs_are_covered_after_substitution() {
        use cosmic_text::fontdb;
        let mut db = fontdb::Database::new();
        for data in [
            include_bytes!("../../../../assets/fonts/Inter-Regular.ttf").as_slice(),
            include_bytes!("../../../../assets/fonts/Literata-Regular.ttf").as_slice(),
        ] {
            db.load_font_data(data.to_vec());
        }
        for (fixture, kind) in [(XDXF, b'x'), (HTML, b'h'), (PANGO, b'g')] {
            for c in parse(fixture, kind).iter().flat_map(|b| b.text.chars()).filter(|c| !c.is_whitespace()) {
                assert!(
                    db.faces().any(|f| db
                        .with_face_data(f.id, |data, index| {
                            use cosmic_text::skrifa::MetadataProvider;
                            cosmic_text::skrifa::FontRef::from_index(data, index).unwrap().charmap().map(c).is_some()
                        })
                        .unwrap_or(false)),
                    "missing {c}"
                );
            }
        }
    }
}
