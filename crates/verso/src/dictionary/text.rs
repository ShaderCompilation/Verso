//! Text-only presentation. No resource loads, styles or script execution.
use anyhow::{Context, Result, ensure};

pub fn definition(mut data: &[u8], sequence: &str) -> Result<String> {
    let mut output = Vec::new();
    let mut fields = sequence.bytes().peekable();
    loop {
        let (kind, last) = if sequence.is_empty() {
            if data.is_empty() {
                break;
            }
            let k = data[0];
            data = &data[1..];
            (k, false)
        } else {
            let Some(k) = fields.next() else {
                break;
            };
            (k, fields.peek().is_none())
        };
        ensure!(kind.is_ascii_alphabetic(), "Invalid definition field type");
        let (field, remaining) = if last {
            (data, &[][..])
        } else if kind.is_ascii_uppercase() {
            let size = u32::from_be_bytes(data.get(..4).context("Truncated binary field")?.try_into()?) as usize;
            let end = 4usize.checked_add(size).context("Field size overflow")?;
            (data.get(4..end).context("Truncated binary field")?, data.get(end..).context("Truncated field")?)
        } else {
            let end = data.iter().position(|b| *b == 0).context("Unterminated text field")?;
            (&data[..end], &data[end + 1..])
        };
        data = remaining;
        if matches!(kind, b'm' | b't' | b'y' | b'h' | b'x' | b'g') {
            let source = std::str::from_utf8(field)?;
            let text = if matches!(kind, b'h' | b'x' | b'g') { markup(source) } else { entities(source) };
            let text = clean(&text);
            if !text.is_empty() {
                output.push(text);
            }
        }
    }
    ensure!(data.is_empty(), "Trailing definition data");
    if output.is_empty() {
        return Ok("This entry has no supported text definition.".into());
    }
    Ok(output.join("\n\n"))
}
fn entities(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.char_indices().take_while(|(i, _)| *i < 64).find_map(|(i, c)| (c == ';').then_some(i));
        if let Some(end) = end {
            let entity = &rest[..=end];
            match quick_xml::escape::unescape(entity) {
                Ok(decoded) => out.push_str(&decoded),
                Err(_) => out.push_str(entity),
            }
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}
fn newline(out: &mut String, count: usize) {
    while out.ends_with(' ') {
        out.pop();
    }
    let existing = out.chars().rev().take_while(|c| *c == '\n').count();
    for _ in existing..count {
        out.push('\n');
    }
}
fn markup(source: &str) -> String {
    let mut out = String::new();
    let mut rest = source;
    let mut hidden: Option<String> = None;
    while !rest.is_empty() {
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |end| &rest[end + 3..]);
            continue;
        }
        if rest.starts_with('<') {
            let mut quote = None;
            let end = rest.char_indices().skip(1).find_map(|(i, c)| {
                if let Some(q) = quote {
                    if c == q {
                        quote = None;
                    }
                } else if c == '\'' || c == '"' {
                    quote = Some(c);
                } else if c == '>' {
                    return Some(i);
                }
                None
            });
            if let Some(end) = end {
                let token = rest[1..end].trim();
                let closing = token.starts_with('/');
                let tag = token
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if let Some(hide) = &hidden {
                    if closing && hide == &tag {
                        hidden = None;
                    }
                } else if matches!(tag.as_str(), "script" | "style" | "rref") && !closing && !token.ends_with('/') {
                    hidden = Some(tag);
                } else if matches!(tag.as_str(), "p" | "div" | "def" | "blockquote" | "h1" | "h2" | "h3" | "k") {
                    newline(&mut out, 2);
                } else if matches!(tag.as_str(), "br" | "li" | "tr" | "ex") {
                    newline(&mut out, 1);
                } else if matches!(tag.as_str(), "td" | "th") && closing {
                    out.push(' ');
                }
                rest = &rest[end + 1..];
                continue;
            }
        }
        let ch = rest.chars().next().unwrap();
        rest = &rest[ch.len_utf8()..];
        if hidden.is_some() {
            continue;
        }
        if ch.is_whitespace() {
            if !out.ends_with([' ', '\n']) {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    entities(&out)
}
fn clean(source: &str) -> String {
    let mut out = String::new();
    for line in source.replace("\r\n", "\n").replace('\r', "\n").lines() {
        let line: String = line.chars().filter(|c| !c.is_control() || *c == '\t').collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paragraphs_entities_and_inert_markup() -> Result<()> {
        assert_eq!(
            definition(b"<p>A &eacute; &amp; &#x416;</p><p>B<br>C</p><script>bad()</script>", "h")?,
            "A é & Ж\n\nB\nC"
        );
        assert_eq!(
            definition(
                b"<ar><k>word</k><def>one <b>two</b></def><def>three &unknown;</def><rref>sound.wav</rref></ar>",
                "x"
            )?,
            "word\n\none two\n\nthree &unknown;"
        );
        assert_eq!(definition(b"a\n\nb &lt;c&gt;", "m")?, "a\n\nb <c>");
        assert_eq!(definition(b"mmeaning\0h<p>other</p>\0W\0\0\0\x02ab", "")?, "meaning\n\nother");
        assert_eq!(definition(b"phonetic\0meaning", "tm")?, "phonetic\n\nmeaning");
        assert!(definition(b"hnot terminated", "").is_err());
        assert!(definition(b"W\xff\xff\xff\xffbad", "").is_err());
        Ok(())
    }
}
