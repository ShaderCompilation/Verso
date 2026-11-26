//! Bounded Markdown inspection and deterministic XHTML rendering.
//!
//! Reads UTF-8 Markdown files up to [`MAX_BYTES`] (stripping a leading BOM and
//! rejecting embedded NUL bytes), parses an optional flat YAML frontmatter block
//! (capped at [`MAX_FRONTMATTER_BYTES`], falling back to normal prose if unclosed
//! or unparseable), extracts [`Metadata`] / synopses / plain-text language
//! samples, validates relative `cover:` paths inside the document's parent
//! directory, and renders CommonMark + GFM + footnotes into deterministic XHTML
//! for crengine.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use pulldown_cmark::{Alignment, CodeBlockKind, CowStr, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd};

use crate::library::Metadata;

/// Maximum Markdown file size Verso will open or index (16 MiB).
pub const MAX_BYTES: usize = 16 * 1024 * 1024;

/// Maximum YAML frontmatter size (64 KiB).
pub const MAX_FRONTMATTER_BYTES: usize = 64 * 1024;

const GFM_OPTIONS: Options = Options::ENABLE_TABLES
    .union(Options::ENABLE_FOOTNOTES)
    .union(Options::ENABLE_STRIKETHROUGH)
    .union(Options::ENABLE_TASKLISTS);

/// Rendered XHTML together with resolved document title and language tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub xhtml: String,
    pub title: String,
    pub language: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum YamlValue {
    Scalar(String),
    List(Vec<String>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Frontmatter {
    fields: BTreeMap<String, YamlValue>,
}

impl Frontmatter {
    fn scalar(&self, key: &str) -> Option<&str> {
        match self.fields.get(key)? {
            YamlValue::Scalar(s) => {
                let s = s.trim();
                (!s.is_empty()).then_some(s)
            }
            YamlValue::List(_) => None,
        }
    }

    fn joined(&self, key: &str) -> Option<String> {
        match self.fields.get(key)? {
            YamlValue::Scalar(s) => {
                let s = s.trim();
                (!s.is_empty()).then(|| s.to_owned())
            }
            YamlValue::List(items) => {
                let joined: Vec<&str> = items.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
                (!joined.is_empty()).then(|| joined.join(", "))
            }
        }
    }
}

/// Validates and normalizes in-memory Markdown bytes: checks [`MAX_BYTES`],
/// rejects embedded NUL bytes and invalid UTF-8, and strips a leading UTF-8 BOM.
pub fn normalize_bytes(bytes: &[u8]) -> Result<&str> {
    ensure!(bytes.len() <= MAX_BYTES, "Markdown file exceeds {MAX_BYTES} bytes");
    ensure!(!bytes.contains(&0), "Markdown file contains NUL bytes");
    let text = std::str::from_utf8(bytes).context("Markdown file is not valid UTF-8")?;
    Ok(text.strip_prefix('\u{FEFF}').unwrap_or(text))
}

/// Reads a bounded UTF-8 Markdown file, stripping a leading UTF-8 BOM and
/// rejecting embedded NUL bytes or files over [`MAX_BYTES`].
pub fn read_file(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    ensure!(meta.len() <= MAX_BYTES as u64, "Markdown file exceeds {MAX_BYTES} bytes");
    let mut bytes = Vec::with_capacity(meta.len().min(MAX_BYTES as u64) as usize);
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    let had_bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    normalize_bytes(&bytes)?;
    if had_bom {
        bytes.drain(..3);
    }
    Ok(String::from_utf8(bytes).expect("validated UTF-8"))
}

/// Validates a Markdown `cover:` reference syntactically as a clean relative
/// path inside the document's folder (no leading `/`, no `..`, no `\`, no URL
/// scheme, no NUL). Returns a normalized `/`-separated relative path.
pub fn sanitize_relative_cover(cover_ref: &str) -> Result<String> {
    let raw = cover_ref.trim();
    ensure!(!raw.is_empty(), "empty cover path");
    ensure!(!raw.contains(['\0', '\\', ':']), "invalid characters in cover path");
    ensure!(!raw.starts_with('/'), "cover path must be relative");
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("cover path cannot traverse parent directories"),
            _ => parts.push(part),
        }
    }
    ensure!(!parts.is_empty(), "empty cover path");
    Ok(parts.join("/"))
}

/// Resolves `cover_ref` relative to `book_path`'s parent directory, rejecting
/// absolute paths, URL schemes, `..` traversal, missing files, directories, and
/// symlinks that escape the parent directory.
pub fn resolve_cover_path(book_path: &Path, cover_ref: &str) -> Result<PathBuf> {
    let rel = sanitize_relative_cover(cover_ref)?;
    let parent = parent_dir(book_path);
    let canon_parent = std::fs::canonicalize(parent).context("canonicalizing book directory")?;
    let candidate = parent.join(&rel);
    let canon_target = std::fs::canonicalize(&candidate).context("resolving cover file")?;
    ensure!(canon_target.starts_with(&canon_parent) && canon_target != canon_parent, "cover escapes book directory");
    ensure!(std::fs::metadata(&canon_target)?.is_file(), "cover is not a regular file");
    Ok(canon_target)
}

fn parent_dir(book_path: &Path) -> &Path {
    book_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."))
}

/// Checks whether a validated relative `cover` path is safe to keep in
/// `Metadata::cover`: rejects escaping symlinks (including intermediate symlink
/// directories or broken symlinks) and directories, while allowing a missing
/// non-escaping file so later additions still match.
fn validated_cover_for_metadata(book_path: Option<&Path>, raw_cover: &str) -> Option<String> {
    let rel = sanitize_relative_cover(raw_cover).ok()?;
    let Some(book_path) = book_path else { return Some(rel) };
    let parent = parent_dir(book_path);
    let Ok(canon_parent) = std::fs::canonicalize(parent) else {
        return Some(rel);
    };
    let mut cur = parent.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    for (idx, part) in parts.iter().enumerate() {
        cur.push(part);
        match std::fs::symlink_metadata(&cur) {
            Ok(_) => {
                let is_last = idx + 1 == parts.len();
                let Ok(canon) = std::fs::canonicalize(&cur) else {
                    return None;
                };
                if !canon.starts_with(&canon_parent) || canon == canon_parent {
                    return None;
                }
                if is_last && !std::fs::metadata(&canon).is_ok_and(|m| m.is_file()) {
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return None,
        }
    }
    Some(rel)
}

/// Reads metadata from a Markdown file without filling an empty title from the
/// filename. Callers reading a temporary `.part` file fill the title from the
/// final filename after renaming.
pub fn read_metadata(path: &Path) -> Result<Metadata> {
    let input = read_file(path)?;
    Ok(extract_metadata(Some(path), &input))
}

/// Reads metadata from a Markdown file, falling back to its filename stem when
/// neither frontmatter nor a level-1 heading provides a title.
pub fn read(path: &Path) -> Result<Metadata> {
    let mut meta = read_metadata(path)?;
    if meta.title.is_empty() {
        meta.title = filename_stem(path);
    }
    Ok(meta)
}

/// Reads metadata directly from validated Markdown bytes (no disk cover check).
pub fn metadata_from_bytes(bytes: &[u8]) -> Result<Metadata> {
    let input = normalize_bytes(bytes)?;
    Ok(extract_metadata(None, input))
}

/// Returns the trimmed plain-text synopsis from frontmatter (`description` or
/// `synopsis`), or `None` when absent or empty.
pub fn synopsis(path: &Path) -> Result<Option<String>> {
    let input = read_file(path)?;
    let (fm, _) = split_frontmatter(&input);
    Ok(extract_synopsis(&fm))
}

/// Extracts up to `limit` bytes of plain prose text from a Markdown file (with
/// frontmatter and syntax stripped) for language detection.
pub fn sample_text(path: &Path, limit: usize) -> Result<String> {
    let input = read_file(path)?;
    Ok(sample_text_from_str(&input, limit))
}

/// Extracts up to `limit` bytes of plain prose text from a Markdown string
/// (with frontmatter and syntax stripped).
pub fn sample_text_from_str(input: &str, limit: usize) -> String {
    if limit == 0 {
        return String::new();
    }
    let (_, body) = split_frontmatter(input);
    // Bound parser work to a safe prefix of the body (at least 16 KiB or 2x limit).
    let scan_budget = limit.saturating_mul(2).max(16 * 1024);
    let prefix = floor_char_boundary_slice(body, scan_budget);
    let mut out = String::new();
    let mut code_depth = 0usize;
    let mut html_depth = 0usize;
    for event in Parser::new_ext(prefix, GFM_OPTIONS) {
        if out.len() >= limit {
            break;
        }
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_depth += 1,
            Event::End(TagEnd::CodeBlock) => code_depth = code_depth.saturating_sub(1),
            Event::Start(Tag::HtmlBlock) => html_depth += 1,
            Event::End(TagEnd::HtmlBlock) => html_depth = html_depth.saturating_sub(1),
            Event::Text(text) if code_depth == 0 && html_depth == 0 => {
                push_bounded_sample(&mut out, &text, limit);
            }
            Event::SoftBreak | Event::HardBreak if code_depth == 0 && html_depth == 0 => {
                push_bounded_sample(&mut out, " ", limit);
            }
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::Item
                | TagEnd::TableRow
                | TagEnd::FootnoteDefinition,
            ) if code_depth == 0 && html_depth == 0 && !out.ends_with([' ', '\n']) && !out.is_empty() => {
                push_bounded_sample(&mut out, "\n", limit);
            }
            _ => {}
        }
    }
    out.trim().to_owned()
}

/// Reads and renders a Markdown file to deterministic XHTML.
pub fn render_file(path: &Path) -> Result<Rendered> {
    let input = read_file(path)?;
    let fallback = filename_stem(path);
    Ok(to_xhtml(&input, &fallback))
}

/// Converts Markdown text (with optional YAML frontmatter) into deterministic
/// XHTML wrapped in `<!DOCTYPE html><html lang="…">...</html>`.
pub fn to_xhtml(input: &str, fallback_title: &str) -> Rendered {
    let (fm, body) = split_frontmatter(input);
    let title = resolve_title(&fm, body, fallback_title);
    let language = fm.scalar("lang").or_else(|| fm.scalar("language")).unwrap_or("en").to_owned();
    let body_html = render_body(body);

    let mut xhtml = String::with_capacity(body_html.len() + 256);
    xhtml.push_str("<!DOCTYPE html><html lang=\"");
    escape_attr(&mut xhtml, &language);
    xhtml.push_str("\"><head><meta charset=\"utf-8\"/><title>");
    escape_text(&mut xhtml, &title);
    xhtml.push_str("</title></head><body>\n");
    xhtml.push_str(&body_html);
    xhtml.push_str("</body></html>");

    Rendered { xhtml, title, language }
}

fn filename_stem(path: &Path) -> String {
    path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
}

fn extract_synopsis(fm: &Frontmatter) -> Option<String> {
    fm.scalar("description").or_else(|| fm.scalar("synopsis")).map(str::to_owned)
}

fn extract_metadata(book_path: Option<&Path>, input: &str) -> Metadata {
    let (fm, body) = split_frontmatter(input);
    let title = resolve_title(&fm, body, "");
    let authors = fm.joined("authors").or_else(|| fm.joined("author")).unwrap_or_default();
    let language = fm.scalar("lang").or_else(|| fm.scalar("language")).unwrap_or_default().to_owned();
    let synopsis = Some(extract_synopsis(&fm).is_some());
    let series = fm.scalar("series").unwrap_or_default().to_owned();
    let series_index = fm.scalar("series_index").unwrap_or_default().to_owned();
    let cover = fm.scalar("cover").and_then(|c| validated_cover_for_metadata(book_path, c));

    Metadata {
        identifier: String::new(),
        title,
        authors,
        language,
        cover,
        synopsis,
        series,
        series_index,
        author_sort: String::new(),
        version: Metadata::VERSION,
    }
}

fn resolve_title(fm: &Frontmatter, body: &str, fallback_title: &str) -> String {
    if let Some(title) = fm.scalar("title") {
        return title.to_owned();
    }
    if let Some(h1) = first_h1_text(body) {
        return h1;
    }
    fallback_title.to_owned()
}

fn first_h1_text(body: &str) -> Option<String> {
    let mut in_h1 = false;
    let mut image_depth = 0usize;
    let mut raw = String::new();
    for event in Parser::new_ext(body, GFM_OPTIONS) {
        match event {
            Event::Start(Tag::Heading { level: HeadingLevel::H1, .. }) if !in_h1 => {
                in_h1 = true;
                raw.clear();
            }
            Event::End(TagEnd::Heading(HeadingLevel::H1)) if in_h1 => {
                in_h1 = false;
                let squashed = squash(&raw);
                if !squashed.is_empty() {
                    return Some(squashed);
                }
            }
            Event::Start(Tag::Image { .. }) if in_h1 => image_depth += 1,
            Event::End(TagEnd::Image) if in_h1 => image_depth = image_depth.saturating_sub(1),
            Event::Text(text) | Event::Code(text) if in_h1 && image_depth == 0 => raw.push_str(&text),
            Event::SoftBreak | Event::HardBreak if in_h1 && image_depth == 0 => raw.push(' '),
            _ => {}
        }
    }
    None
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn floor_char_boundary_slice(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn push_bounded_sample(out: &mut String, piece: &str, limit: usize) {
    let remaining = limit.saturating_sub(out.len());
    if remaining == 0 {
        return;
    }
    out.push_str(floor_char_boundary_slice(piece, remaining));
}

// ---------------------------------------------------------------------------
// YAML frontmatter parser (flat scalars + simple lists)
// ---------------------------------------------------------------------------

fn split_frontmatter(input: &str) -> (Frontmatter, &str) {
    let Some(first_end) = line_end_with_delimiter(input, 0, "---") else {
        return (Frontmatter::default(), input);
    };
    let yaml_start = first_end;
    let mut pos = yaml_start;
    let mut closing: Option<(usize, usize)> = None;

    while pos <= input.len() {
        if pos - yaml_start > MAX_FRONTMATTER_BYTES {
            return (Frontmatter::default(), input);
        }
        if pos == input.len() {
            break;
        }
        let line_start = pos;
        let (line, next_pos) = next_line(input, pos);
        let trimmed = line.trim_matches([' ', '\t']);
        if trimmed == "---" || trimmed == "..." {
            closing = Some((line_start, next_pos));
            break;
        }
        if next_pos - yaml_start > MAX_FRONTMATTER_BYTES {
            return (Frontmatter::default(), input);
        }
        pos = next_pos;
    }

    let Some((yaml_end, body_start)) = closing else {
        return (Frontmatter::default(), input);
    };

    let yaml = &input[yaml_start..yaml_end];
    match parse_yaml_subset(yaml) {
        Ok(fm) => (fm, &input[body_start..]),
        Err(_) => (Frontmatter::default(), input),
    }
}

/// Checks if the line at `offset` equals `delimiter` (with optional horizontal
/// whitespace) AND is terminated by `\n` or `\r\n`. Returns the byte offset of
/// the following line.
fn line_end_with_delimiter(input: &str, offset: usize, delimiter: &str) -> Option<usize> {
    let slice = &input[offset..];
    let nl = slice.find('\n')?;
    let line = slice[..nl].strip_suffix('\r').unwrap_or(&slice[..nl]);
    (line.trim_matches([' ', '\t']) == delimiter).then_some(offset + nl + 1)
}

fn next_line(input: &str, offset: usize) -> (&str, usize) {
    let slice = &input[offset..];
    match slice.find('\n') {
        Some(nl) => {
            let line = slice[..nl].strip_suffix('\r').unwrap_or(&slice[..nl]);
            (line, offset + nl + 1)
        }
        None => {
            let line = slice.strip_suffix('\r').unwrap_or(slice);
            (line, input.len())
        }
    }
}

fn parse_yaml_subset(yaml: &str) -> Result<Frontmatter> {
    let mut fields = BTreeMap::new();
    let mut current_list_key: Option<String> = None;

    for raw_line in yaml.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let trimmed_end = line.trim_end_matches([' ', '\t']);
        let trimmed = trimmed_end.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed == "-" || trimmed.starts_with("- ") || trimmed.starts_with("-\t") {
            let key = current_list_key.as_deref().context("YAML list item without key")?;
            let item_raw = trimmed[1..].trim_start_matches([' ', '\t']);
            let item = if item_raw.is_empty() || item_raw.starts_with('#') {
                String::new()
            } else {
                parse_yaml_scalar(item_raw)?
            };
            match fields.get_mut(key) {
                Some(YamlValue::List(items)) => items.push(item),
                _ => bail!("YAML list item following scalar key"),
            }
            continue;
        }

        // Top-level keys cannot be indented; nested YAML blocks fall back to prose.
        ensure!(!line.starts_with([' ', '\t']), "nested YAML keys are unsupported");
        let (key, rest) = split_yaml_key_value(trimmed_end)?;
        let val = rest.trim_start_matches([' ', '\t']);
        if val.is_empty() || val.starts_with('#') {
            fields.insert(key.clone(), YamlValue::List(Vec::new()));
            current_list_key = Some(key);
        } else if val.starts_with('[') {
            current_list_key = None;
            let items = parse_inline_list(val)?;
            fields.insert(key, YamlValue::List(items));
        } else {
            current_list_key = None;
            let scalar = parse_yaml_scalar(val)?;
            fields.insert(key, YamlValue::Scalar(scalar));
        }
    }

    Ok(Frontmatter { fields })
}

fn split_yaml_key_value(line: &str) -> Result<(String, &str)> {
    let colon = line.find(':').context("expected key: value")?;
    let key = &line[..colon];
    let after = &line[colon + 1..];
    ensure!(after.is_empty() || after.starts_with([' ', '\t']), "YAML colon must precede space or end of line");
    ensure!(
        !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')),
        "invalid YAML key"
    );
    Ok((key.to_ascii_lowercase(), after))
}

fn parse_yaml_scalar(raw: &str) -> Result<String> {
    let raw = raw.trim_start_matches([' ', '\t']);
    if let Some(rest) = raw.strip_prefix('"') {
        return parse_double_quoted(rest).map(|(s, _)| s);
    }
    if let Some(rest) = raw.strip_prefix('\'') {
        return parse_single_quoted(rest).map(|(s, _)| s);
    }
    ensure!(!raw.starts_with(['{', '[']), "unsupported YAML collection");
    Ok(strip_unquoted_comment(raw).trim_end_matches([' ', '\t']).to_owned())
}

fn parse_double_quoted(s: &str) -> Result<(String, &str)> {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '"' => {
                let tail = &s[idx + 1..];
                ensure_trailing_comment_or_empty(tail)?;
                return Ok((out, tail));
            }
            '\\' => {
                let (_, esc) = chars.next().context("unterminated YAML escape")?;
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            _ => out.push(ch),
        }
    }
    bail!("unterminated double-quoted YAML string")
}

fn parse_single_quoted(s: &str) -> Result<(String, &str)> {
    let mut out = String::new();
    let mut chars = s.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch == '\'' {
            if let Some(&(_, '\'')) = chars.peek() {
                chars.next();
                out.push('\'');
            } else {
                let tail = &s[idx + 1..];
                ensure_trailing_comment_or_empty(tail)?;
                return Ok((out, tail));
            }
        } else {
            out.push(ch);
        }
    }
    bail!("unterminated single-quoted YAML string")
}

fn ensure_trailing_comment_or_empty(tail: &str) -> Result<()> {
    let t = tail.trim_start_matches([' ', '\t']);
    ensure!(t.is_empty() || t.starts_with('#'), "unexpected trailing characters after YAML string");
    Ok(())
}

fn strip_unquoted_comment(s: &str) -> &str {
    let bytes = s.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] == b'#' && matches!(bytes[i - 1], b' ' | b'\t') {
            return &s[..i - 1];
        }
    }
    s
}

fn parse_inline_list(s: &str) -> Result<Vec<String>> {
    let inner_start = s.strip_prefix('[').context("expected '['")?;
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut close_idx = None;

    for (idx, ch) in inner_start.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_double {
            match ch {
                '\\' => escaped = true,
                '"' => in_double = false,
                _ => {}
            }
        } else if in_single {
            if ch == '\'' {
                in_single = false;
            }
        } else {
            match ch {
                '"' => in_double = true,
                '\'' => in_single = true,
                ']' => {
                    close_idx = Some(idx);
                    break;
                }
                '[' => bail!("nested YAML inline lists are unsupported"),
                _ => {}
            }
        }
    }

    let close = close_idx.context("unterminated YAML inline list")?;
    ensure_trailing_comment_or_empty(&inner_start[close + 1..])?;
    let interior = &inner_start[..close];
    if interior.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut parts = Vec::new();
    let mut start = 0usize;
    in_single = false;
    in_double = false;
    escaped = false;
    for (idx, ch) in interior.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_double {
            match ch {
                '\\' => escaped = true,
                '"' => in_double = false,
                _ => {}
            }
        } else if in_single {
            if ch == '\'' {
                in_single = false;
            }
        } else {
            match ch {
                '"' => in_double = true,
                '\'' => in_single = true,
                ',' => {
                    parts.push(&interior[start..idx]);
                    start = idx + 1;
                }
                _ => {}
            }
        }
    }
    parts.push(&interior[start..]);

    let mut items = Vec::with_capacity(parts.len());
    for (idx, part) in parts.iter().enumerate() {
        let trimmed = part.trim();
        if trimmed.is_empty() && idx + 1 == parts.len() {
            // Allow trailing comma: `[a, b,]`.
            break;
        }
        ensure!(!trimmed.is_empty(), "empty element in YAML inline list");
        items.push(parse_inline_list_item(trimmed)?);
    }
    Ok(items)
}

fn parse_inline_list_item(raw: &str) -> Result<String> {
    if let Some(rest) = raw.strip_prefix('"') {
        let (val, tail) = parse_double_quoted(rest)?;
        ensure!(tail.trim().is_empty(), "unexpected text in YAML list item");
        return Ok(val);
    }
    if let Some(rest) = raw.strip_prefix('\'') {
        let (val, tail) = parse_single_quoted(rest)?;
        ensure!(tail.trim().is_empty(), "unexpected text in YAML list item");
        return Ok(val);
    }
    Ok(raw.to_owned())
}

// ---------------------------------------------------------------------------
// Deterministic XHTML renderer
// ---------------------------------------------------------------------------

struct FootnoteDef<'a> {
    label: String,
    events: Vec<Event<'a>>,
}

struct FootnoteTracker {
    defined: HashMap<String, usize>,
    ref_number: HashMap<String, usize>,
    ref_counts: HashMap<usize, usize>,
    ref_order: Vec<usize>,
}

impl FootnoteTracker {
    fn new(defs: &[FootnoteDef<'_>]) -> Self {
        let mut defined = HashMap::new();
        for (idx, def) in defs.iter().enumerate() {
            defined.entry(def.label.clone()).or_insert(idx);
        }
        Self { defined, ref_number: HashMap::new(), ref_counts: HashMap::new(), ref_order: Vec::new() }
    }

    /// Assigns/looks up a sequential 1-based footnote number and unique `fnref`
    /// occurrence id for a referenced label. Returns `None` if undefined.
    fn reference(&mut self, label: &str) -> Option<(usize, usize)> {
        let &def_idx = self.defined.get(label)?;
        let num = if let Some(&num) = self.ref_number.get(label) {
            num
        } else {
            let num = self.ref_order.len() + 1;
            self.ref_number.insert(label.to_owned(), num);
            self.ref_order.push(def_idx);
            num
        };
        let occ = self.ref_counts.entry(num).and_modify(|c| *c += 1).or_insert(1);
        Some((num, *occ))
    }
}

fn render_body(body: &str) -> String {
    let all_events: Vec<Event<'_>> = Parser::new_ext(body, GFM_OPTIONS).collect();
    let (main_events, defs) = partition_footnotes(all_events);
    let mut tracker = FootnoteTracker::new(&defs);

    let mut out = String::with_capacity(body.len() + body.len() / 4);
    render_events(&main_events, &mut tracker, &mut out);

    // Render referenced footnote definitions in order of first reference (a loop
    // handles footnotes referencing other footnotes), followed by any unreferenced
    // definitions.
    let mut rendered_defs = vec![false; defs.len()];
    let mut endnotes = String::new();
    let mut cursor = 0usize;
    while cursor < tracker.ref_order.len() {
        let def_idx = tracker.ref_order[cursor];
        let num = cursor + 1;
        cursor += 1;
        rendered_defs[def_idx] = true;
        write_footnote_aside(num, &defs[def_idx].events, &mut tracker, &mut endnotes);
    }
    let mut next_num = tracker.ref_order.len() + 1;
    for (def_idx, def) in defs.iter().enumerate() {
        if !rendered_defs[def_idx] {
            write_footnote_aside(next_num, &def.events, &mut tracker, &mut endnotes);
            next_num += 1;
        }
    }

    if !endnotes.is_empty() {
        out.push_str("<section role=\"doc-endnotes\">\n<hr/>\n");
        out.push_str(&endnotes);
        out.push_str("</section>\n");
    }
    out
}

fn write_footnote_aside<'a>(num: usize, events: &[Event<'a>], tracker: &mut FootnoteTracker, out: &mut String) {
    use std::fmt::Write;
    let _ = writeln!(out, "<aside id=\"fn-{num}\" role=\"doc-footnote\">");
    render_events(events, tracker, out);
    out.push_str("</aside>\n");
}

fn partition_footnotes<'a>(events: Vec<Event<'a>>) -> (Vec<Event<'a>>, Vec<FootnoteDef<'a>>) {
    let mut main = Vec::with_capacity(events.len());
    let mut defs = Vec::new();
    let mut iter = events.into_iter();
    while let Some(event) = iter.next() {
        match event {
            Event::Start(Tag::FootnoteDefinition(label)) => {
                let mut fn_events = Vec::new();
                let mut depth = 1usize;
                for inner in iter.by_ref() {
                    match &inner {
                        Event::Start(Tag::FootnoteDefinition(_)) => depth += 1,
                        Event::End(TagEnd::FootnoteDefinition) => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    fn_events.push(inner);
                }
                defs.push(FootnoteDef { label: label.into_string(), events: fn_events });
            }
            other => main.push(other),
        }
    }
    (main, defs)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TableState {
    Head,
    Body,
}

struct TableContext {
    state: TableState,
    alignments: Vec<Alignment>,
    cell_index: usize,
}

fn render_events<'a>(events: &[Event<'a>], footnotes: &mut FootnoteTracker, out: &mut String) {
    let mut table_stack: Vec<TableContext> = Vec::new();
    let mut i = 0usize;

    while i < events.len() {
        match &events[i] {
            Event::Start(tag) => match tag {
                Tag::Paragraph => out.push_str("<p>"),
                Tag::Heading { level, .. } => {
                    out.push('<');
                    out.push_str(heading_tag(*level));
                    out.push('>');
                }
                Tag::BlockQuote(_) => out.push_str("<blockquote>\n"),
                Tag::CodeBlock(kind) => {
                    out.push_str("<pre><code");
                    if let CodeBlockKind::Fenced(info) = kind {
                        let lang = info.split_whitespace().next().unwrap_or("");
                        if !lang.is_empty() {
                            out.push_str(" class=\"language-");
                            escape_attr(out, lang);
                            out.push('"');
                        }
                    }
                    out.push('>');
                }
                Tag::HtmlBlock => out.push_str("<p>"),
                Tag::List(None) => out.push_str("<ul>\n"),
                Tag::List(Some(1)) => out.push_str("<ol>\n"),
                Tag::List(Some(start)) => {
                    use std::fmt::Write;
                    let _ = writeln!(out, "<ol start=\"{start}\">");
                }
                Tag::Item => {
                    let is_task = matches!(events.get(i + 1), Some(Event::TaskListMarker(_)))
                        || (matches!(events.get(i + 1), Some(Event::Start(Tag::Paragraph)))
                            && matches!(events.get(i + 2), Some(Event::TaskListMarker(_))));
                    if is_task {
                        out.push_str("<li class=\"task-item\">");
                    } else {
                        out.push_str("<li>");
                    }
                }
                Tag::FootnoteDefinition(_) => {}
                Tag::DefinitionList => out.push_str("<dl>\n"),
                Tag::DefinitionListTitle => out.push_str("<dt>"),
                Tag::DefinitionListDefinition => out.push_str("<dd>"),
                Tag::Table(alignments) => {
                    table_stack.push(TableContext {
                        state: TableState::Head,
                        alignments: alignments.clone(),
                        cell_index: 0,
                    });
                    out.push_str("<table>\n");
                }
                Tag::TableHead => {
                    if let Some(ctx) = table_stack.last_mut() {
                        ctx.state = TableState::Head;
                        ctx.cell_index = 0;
                    }
                    out.push_str("<thead><tr>");
                }
                Tag::TableRow => {
                    if let Some(ctx) = table_stack.last_mut() {
                        ctx.cell_index = 0;
                    }
                    out.push_str("<tr>");
                }
                Tag::TableCell => {
                    let (tag_name, align) = table_stack.last().map_or(("td", Alignment::None), |ctx| {
                        let tag_name = match ctx.state {
                            TableState::Head => "th",
                            TableState::Body => "td",
                        };
                        let align = ctx.alignments.get(ctx.cell_index).copied().unwrap_or(Alignment::None);
                        (tag_name, align)
                    });
                    out.push('<');
                    out.push_str(tag_name);
                    match align {
                        Alignment::Left => out.push_str(" style=\"text-align: left\""),
                        Alignment::Center => out.push_str(" style=\"text-align: center\""),
                        Alignment::Right => out.push_str(" style=\"text-align: right\""),
                        Alignment::None => {}
                    }
                    out.push('>');
                }
                Tag::Emphasis => out.push_str("<em>"),
                Tag::Strong => out.push_str("<strong>"),
                Tag::Strikethrough => out.push_str("<del>"),
                Tag::Superscript => out.push_str("<sup>"),
                Tag::Subscript => out.push_str("<sub>"),
                Tag::Link { link_type, dest_url, title, .. } => {
                    out.push_str("<a href=\"");
                    if *link_type == LinkType::Email {
                        out.push_str("mailto:");
                    }
                    escape_attr(out, sanitize_href(dest_url));
                    if !title.is_empty() {
                        out.push_str("\" title=\"");
                        escape_attr(out, title);
                    }
                    out.push_str("\">");
                }
                Tag::Image { dest_url, title, .. } => {
                    let (alt, consumed) = collect_image_alt(&events[i + 1..]);
                    out.push_str("<img src=\"");
                    escape_attr(out, sanitize_href(dest_url));
                    out.push_str("\" alt=\"");
                    escape_attr(out, &alt);
                    if !title.is_empty() {
                        out.push_str("\" title=\"");
                        escape_attr(out, title);
                    }
                    out.push_str("\"/>");
                    i += consumed;
                }
                Tag::MetadataBlock(_) => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Paragraph => out.push_str("</p>\n"),
                TagEnd::Heading(level) => {
                    out.push_str("</");
                    out.push_str(heading_tag(*level));
                    out.push_str(">\n");
                }
                TagEnd::BlockQuote(_) => out.push_str("</blockquote>\n"),
                TagEnd::CodeBlock => out.push_str("</code></pre>\n"),
                TagEnd::HtmlBlock => out.push_str("</p>\n"),
                TagEnd::List(true) => out.push_str("</ol>\n"),
                TagEnd::List(false) => out.push_str("</ul>\n"),
                TagEnd::Item => out.push_str("</li>\n"),
                TagEnd::FootnoteDefinition => {}
                TagEnd::DefinitionList => out.push_str("</dl>\n"),
                TagEnd::DefinitionListTitle => out.push_str("</dt>\n"),
                TagEnd::DefinitionListDefinition => out.push_str("</dd>\n"),
                TagEnd::Table => {
                    table_stack.pop();
                    out.push_str("</tbody></table>\n");
                }
                TagEnd::TableHead => {
                    if let Some(ctx) = table_stack.last_mut() {
                        ctx.state = TableState::Body;
                    }
                    out.push_str("</tr></thead><tbody>\n");
                }
                TagEnd::TableRow => out.push_str("</tr>\n"),
                TagEnd::TableCell => {
                    let tag_name = if let Some(ctx) = table_stack.last_mut() {
                        ctx.cell_index += 1;
                        match ctx.state {
                            TableState::Head => "th",
                            TableState::Body => "td",
                        }
                    } else {
                        "td"
                    };
                    out.push_str("</");
                    out.push_str(tag_name);
                    out.push('>');
                }
                TagEnd::Emphasis => out.push_str("</em>"),
                TagEnd::Strong => out.push_str("</strong>"),
                TagEnd::Strikethrough => out.push_str("</del>"),
                TagEnd::Superscript => out.push_str("</sup>"),
                TagEnd::Subscript => out.push_str("</sub>"),
                TagEnd::Link => out.push_str("</a>"),
                TagEnd::Image => {}
                TagEnd::MetadataBlock(_) => {}
            },
            Event::Text(text) => escape_text(out, text),
            Event::Code(code) => {
                out.push_str("<code>");
                escape_text(out, code);
                out.push_str("</code>");
            }
            // Escape raw HTML blocks and inline HTML as literal text.
            Event::Html(html) | Event::InlineHtml(html) => escape_text(out, html),
            Event::InlineMath(math) | Event::DisplayMath(math) => escape_text(out, math),
            Event::FootnoteReference(label) => {
                use std::fmt::Write;
                if let Some((num, occ)) = footnotes.reference(label) {
                    if occ == 1 {
                        let _ = write!(
                            out,
                            "<sup><a id=\"fnref-{num}\" href=\"#fn-{num}\" role=\"doc-noteref\">{num}</a></sup>"
                        );
                    } else {
                        let _ = write!(
                            out,
                            "<sup><a id=\"fnref-{num}-{occ}\" href=\"#fn-{num}\" role=\"doc-noteref\">{num}</a></sup>"
                        );
                    }
                } else {
                    out.push('[');
                    escape_text(out, label);
                    out.push(']');
                }
            }
            Event::SoftBreak => out.push('\n'),
            Event::HardBreak => out.push_str("<br/>\n"),
            Event::Rule => out.push_str("<hr/>\n"),
            Event::TaskListMarker(checked) => {
                out.push_str(if *checked { "☑ " } else { "☐ " });
            }
        }
        i += 1;
    }
}

fn collect_image_alt<'a>(rest: &[Event<'a>]) -> (String, usize) {
    let mut alt = String::new();
    let mut nest = 0usize;
    for (idx, event) in rest.iter().enumerate() {
        match event {
            Event::Start(_) => nest += 1,
            Event::End(_) => {
                if nest == 0 {
                    return (alt, idx + 1);
                }
                nest -= 1;
            }
            Event::Text(t) | Event::Code(t) | Event::InlineHtml(t) => alt.push_str(t),
            Event::SoftBreak | Event::HardBreak | Event::Rule => alt.push(' '),
            Event::FootnoteReference(label) => {
                alt.push('[');
                alt.push_str(label);
                alt.push(']');
            }
            Event::TaskListMarker(true) => alt.push_str("[x]"),
            Event::TaskListMarker(false) => alt.push_str("[ ]"),
            Event::Html(_) | Event::InlineMath(_) | Event::DisplayMath(_) => {}
        }
    }
    (alt, rest.len())
}

fn heading_tag(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "h1",
        HeadingLevel::H2 => "h2",
        HeadingLevel::H3 => "h3",
        HeadingLevel::H4 => "h4",
        HeadingLevel::H5 => "h5",
        HeadingLevel::H6 => "h6",
    }
}

fn sanitize_href(url: &str) -> &str {
    let trimmed = url.trim_start_matches(|c: char| c.is_ascii_whitespace() || c.is_ascii_control());
    let lower: String = trimmed.chars().take(16).map(|c| c.to_ascii_lowercase()).collect();
    if lower.starts_with("javascript:") || lower.starts_with("vbscript:") || lower.starts_with("data:text/html") {
        "#"
    } else {
        url
    }
}

fn escape_text(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn escape_attr(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
}

#[allow(dead_code)]
fn _unused_cow_check(_: CowStr<'_>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    #[test]
    fn frontmatter_scalars_and_lists_and_fixtures() -> Result<()> {
        let rich = read(&fixture_path("rich.md"))?;
        assert_eq!(rich.title, "Field Notes on E-Ink Typography");
        assert_eq!(rich.authors, "Ada Lovelace, Charles Babbage");
        assert_eq!(rich.language, "en");
        assert_eq!(rich.series, "Verso Guides");
        assert_eq!(rich.series_index, "1");
        assert_eq!(rich.cover.as_deref(), Some("markdown-cover.png"));
        assert_eq!(rich.synopsis, Some(true));
        assert!(synopsis(&fixture_path("rich.md"))?.unwrap().contains("Practical notes"));

        let prose = read(&fixture_path("prose.markdown"))?;
        assert_eq!(prose.title, "Plain Prose Notes");
        assert_eq!(prose.authors, "");
        assert_eq!(prose.synopsis, Some(false));
        assert_eq!(synopsis(&fixture_path("prose.markdown"))?, None);

        // Inline list and single/double-quoted scalars.
        let inline = "---\r\ntitle: \"Part 1: 'Quotes' & Colons\"\r\nauthors: [One, \"Two, Jr.\", 'Three''s']\r\nlang: de\r\nsynopsis: 'Short summary'\r\n...\r\nBody\r\n";
        let meta = metadata_from_bytes(inline.as_bytes())?;
        assert_eq!(meta.title, "Part 1: 'Quotes' & Colons");
        assert_eq!(meta.authors, "One, Two, Jr., Three's");
        assert_eq!(meta.language, "de");
        assert_eq!(meta.synopsis, Some(true));
        Ok(())
    }

    #[test]
    fn title_fallback_chain_frontmatter_then_h1_then_filename() -> Result<()> {
        let dir = tempfile::tempdir()?;

        let with_fm = dir.path().join("fm.md");
        std::fs::write(&with_fm, "---\ntitle: From Frontmatter\n---\n# Heading Title\n")?;
        assert_eq!(read_metadata(&with_fm)?.title, "From Frontmatter");
        assert_eq!(read(&with_fm)?.title, "From Frontmatter");

        let with_h1 = dir.path().join("h1.md");
        std::fs::write(&with_h1, "---\nauthor: Someone\n---\n\n# First *Formatted* `Heading`\n\n## Second\n")?;
        assert_eq!(read_metadata(&with_h1)?.title, "First Formatted Heading");
        assert_eq!(read(&with_h1)?.title, "First Formatted Heading");

        let plain = dir.path().join("untitled-notes.markdown");
        std::fs::write(&plain, "## Subheading only\n\nJust prose.\n")?;
        assert_eq!(read_metadata(&plain)?.title, "");
        assert_eq!(read(&plain)?.title, "untitled-notes");
        Ok(())
    }

    #[test]
    fn unclosed_or_malformed_frontmatter_falls_back_to_prose() {
        let unclosed = "---\ntitle: Never Closed\n\n# Real Heading\n\nParagraph.\n";
        let rendered = to_xhtml(unclosed, "fallback");
        assert_eq!(rendered.title, "Real Heading");
        assert!(rendered.xhtml.contains("<hr/>"));
        assert!(rendered.xhtml.contains("title: Never Closed"));

        let prose_between_rules = "---\nThis line is prose, not YAML.\n---\n\nBody text.\n";
        let rendered = to_xhtml(prose_between_rules, "fallback");
        assert_eq!(rendered.title, "fallback");
        assert!(rendered.xhtml.contains("This line is prose, not YAML."));

        let unclosed_quote = "---\ntitle: \"oops\n---\n# After\n";
        let rendered = to_xhtml(unclosed_quote, "fallback");
        assert_eq!(rendered.title, "After");
        assert!(rendered.xhtml.contains("title: \"oops"));

        // Over 64 KiB frontmatter without a closer in range falls back to prose.
        let huge_fm = format!("---\ntitle: {}\n---\n# Title\n", "a".repeat(MAX_FRONTMATTER_BYTES + 10));
        let rendered = to_xhtml(&huge_fm, "fallback");
        assert_eq!(rendered.title, "Title");
        assert!(rendered.xhtml.contains("<hr/>"));
    }

    #[test]
    fn cover_validation_rejects_traversal_absolute_urls_and_escaping_symlinks() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let book_dir = dir.path().join("books");
        let outside = dir.path().join("secret.png");
        std::fs::create_dir_all(book_dir.join("images"))?;
        std::fs::write(&outside, b"secret")?;
        std::fs::write(book_dir.join("images/ok.png"), b"ok")?;
        let book_path = book_dir.join("notes.md");

        for bad in [
            "",
            "../secret.png",
            "images/../../secret.png",
            "/etc/passwd",
            "https://example.com/cover.png",
            "file:///tmp/cover.png",
            "images\\ok.png",
            "C:cover.png",
            ".",
        ] {
            assert!(sanitize_relative_cover(bad).is_err(), "should reject {bad:?}");
            assert!(resolve_cover_path(&book_path, bad).is_err(), "should reject {bad:?}");
            let md = format!("---\ncover: {bad}\n---\n# Title\n");
            std::fs::write(&book_path, md)?;
            assert_eq!(read_metadata(&book_path)?.cover, None, "metadata should drop {bad:?}");
        }

        // Valid relative path inside parent directory.
        assert_eq!(sanitize_relative_cover("./images/ok.png")?, "images/ok.png");
        assert_eq!(
            resolve_cover_path(&book_path, "images/ok.png")?,
            std::fs::canonicalize(book_dir.join("images/ok.png"))?
        );
        std::fs::write(&book_path, "---\ncover: ./images/ok.png\n---\n# Title\n")?;
        assert_eq!(read_metadata(&book_path)?.cover.as_deref(), Some("images/ok.png"));

        // Symlink escaping the parent directory is rejected.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, book_dir.join("escape.png"))?;
            assert!(resolve_cover_path(&book_path, "escape.png").is_err());
            std::fs::write(&book_path, "---\ncover: escape.png\n---\n# Title\n")?;
            assert_eq!(read_metadata(&book_path)?.cover, None);

            std::os::unix::fs::symlink(dir.path(), book_dir.join("escape_dir"))?;
            std::fs::write(&book_path, "---\ncover: escape_dir/secret.png\n---\n# Title\n")?;
            assert_eq!(read_metadata(&book_path)?.cover, None);
        }
        Ok(())
    }

    #[test]
    fn bom_stripping_and_binary_or_oversize_rejection() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let bom_path = dir.path().join("bom.md");
        let mut content = vec![0xEF, 0xBB, 0xBF];
        content.extend_from_slice(b"---\ntitle: BOM Title\n---\n# Ignored\nHello");
        std::fs::write(&bom_path, &content)?;
        assert_eq!(read(&bom_path)?.title, "BOM Title");
        assert!(render_file(&bom_path)?.xhtml.contains("<p>Hello</p>"));

        let nul_path = dir.path().join("nul.md");
        std::fs::write(&nul_path, b"hello\0world")?;
        assert!(read(&nul_path).is_err());

        let bad_utf8 = dir.path().join("latin1.md");
        std::fs::write(&bad_utf8, [0xFF, 0xFE, b'a'])?;
        assert!(read(&bad_utf8).is_err());

        let mut too_big = vec![b'a'; MAX_BYTES + 1];
        too_big[MAX_BYTES] = b'\n';
        assert!(normalize_bytes(&too_big).is_err());
        Ok(())
    }

    #[test]
    fn deterministic_xhtml_gfm_raw_html_escaping_and_footnotes() -> Result<()> {
        let input = std::fs::read_to_string(fixture_path("rich.md"))?;
        let first = to_xhtml(&input, "fallback");
        let second = to_xhtml(&input, "fallback");
        assert_eq!(first, second, "XHTML output must be deterministic");

        let html = &first.xhtml;
        assert!(html.starts_with("<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"/><title>Field Notes on E-Ink Typography</title></head><body>"));
        assert!(html.ends_with("</body></html>"));
        assert!(html.contains("<del>deprecated</del>"));
        assert!(html.contains("<table>"));
        assert!(html.contains("<th style=\"text-align: left\">Element</th>"));
        assert!(html.contains("<th style=\"text-align: center\">Alignment</th>"));
        assert!(html.contains("<th style=\"text-align: right\">Behavior</th>"));
        assert!(html.contains("<li class=\"task-item\">☑ Verify monospace code preservation</li>"));
        assert!(html.contains("<li class=\"task-item\">☐ Inspect wrapped table cells on Kindle</li>"));
        assert!(!html.contains("<input"));
        assert!(html.contains("<img src=\"markdown-cover.png\" alt=\"Checkerboard sample\"/>"));
        assert!(html.contains("<sup><a id=\"fnref-1\" href=\"#fn-1\" role=\"doc-noteref\">1</a></sup>"));
        assert!(html.contains("<sup><a id=\"fnref-2\" href=\"#fn-2\" role=\"doc-noteref\">2</a></sup>"));
        assert!(html.contains("<section role=\"doc-endnotes\">"));
        assert!(html.contains("<aside id=\"fn-1\" role=\"doc-footnote\">"));
        assert!(html.contains("<aside id=\"fn-2\" role=\"doc-footnote\">"));

        // Raw HTML blocks and inline HTML are escaped as text, never emitted as tags.
        let xss = "<script>alert(1)</script>\n\nInline <span onclick=\"x\">tag</span> & ampersand.\n";
        let rendered = to_xhtml(xss, "Safe");
        assert!(!rendered.xhtml.contains("<script>"));
        assert!(!rendered.xhtml.contains("<span"));
        assert!(rendered.xhtml.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(rendered.xhtml.contains("&lt;span onclick=\"x\"&gt;tag&lt;/span&gt; &amp; ampersand."));

        // Sample text strips frontmatter, markup, and code blocks cleanly.
        let sample = sample_text(&fixture_path("rich.md"), 256)?;
        assert!(!sample.contains("Ada Lovelace"));
        assert!(!sample.contains("very_long_identifier"));
        assert!(sample.contains("Paper-like screens"));
        Ok(())
    }
}
