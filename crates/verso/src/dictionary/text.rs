//! StarDict field framing. Markup never loads resources or executes scripts.
use super::{Block, BlockKind, markup};
use anyhow::{Context, Result, ensure};

pub fn definition(mut data: &[u8], sequence: &str) -> Result<Vec<Block>> {
    let mut output = Vec::new();
    let mut first_k = true;
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
            let blocks = markup::parse_field(source, kind, &mut first_k);
            if !output.is_empty() && !blocks.is_empty() {
                output.push(Block { kind: BlockKind::Gap, ..Block::plain("") });
            }
            output.extend(blocks);
        }
    }
    ensure!(data.is_empty(), "Trailing definition data");
    if output.is_empty() {
        return Ok(vec![Block::plain("This entry has no supported text definition.")]);
    }
    markup::normalize(&mut output);
    Ok(output)
}
pub(super) fn entities(source: &str) -> String {
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
                Err(_) => out.push_str(super::entities::additional(&entity[1..entity.len() - 1]).unwrap_or(entity)),
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

#[cfg(test)]
mod tests {
    use super::*;
    fn plain(data: &[u8], sequence: &str) -> Result<String> {
        Ok(definition(data, sequence)?.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n"))
    }
    #[test]
    fn framing_entities_and_errors() -> Result<()> {
        assert_eq!(plain(b"<p>A &eacute; &amp; &#x416;</p><p>B<br>C</p><script>bad()</script>", "h")?, "A é & Ж\nB\nC");
        assert_eq!(plain(b"a\n\nb &lt;c&gt; &unknown;", "m")?, "a\n\nb <c> &unknown;");
        assert_eq!(plain(b"mmeaning\0h<p>other</p>\0W\0\0\0\x02ab", "")?, "meaning\n\nother");
        assert_eq!(plain(b"phonetic\0meaning", "tm")?, "phonetic\n\nmeaning");
        assert_eq!(plain(b"\0\0\0\x02abword", "Wm")?, "word");
        assert_eq!(plain(b"W\0\0\0\x02ab", "")?, "This entry has no supported text definition.");
        assert_eq!(plain(b"ab", "W")?, "This entry has no supported text definition.");
        for (data, sequence) in [
            (b"hnot terminated".as_slice(), ""),
            (b"W\xff\xff\xff\xffbad", ""),
            (b"\xff", "m"),
            (b"", "Wm"),
            (b"!", ""),
        ] {
            assert!(definition(data, sequence).is_err(), "{data:?}, {sequence}");
        }
        assert_eq!(plain(b"<k>first</k><blockquote>one</blockquote>\0<k>later</k>two", "xx")?, "one\n\nlatertwo");
        Ok(())
    }
}
