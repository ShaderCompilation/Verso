//! Release notes as styled text, for the update offer. The format
//! (`dist/release-notes/<version>.txt`) is plain text that reads as it is:
//! a line `## Heading` is a section heading, `- ` starts a bullet (its
//! wrapped lines hang under its text), and a blank line separates
//! paragraphs. Other lines continue the paragraph or bullet above them.
use crate::{
    geom::Rect,
    gfx::{StyledBlock, StyledParagraph, Weight},
    screens::paged_text::PagedText,
    ui::theme::{INK2, Theme},
};

/// Shown for a release without notes.
const NO_NOTES: &str = "No notes for this release.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Heading,
    Paragraph,
    Bullet,
}

/// The notes as paragraphs, headings and bullets, with gaps between them.
pub fn blocks(notes: &str, t: &Theme) -> Vec<StyledBlock> {
    let mut items: Vec<(Kind, String)> = Vec::new();
    // The last item takes the lines that follow it until a blank line.
    let mut open = false;
    for line in notes.lines().map(str::trim) {
        if line.is_empty() {
            open = false;
        } else if let Some(heading) = line.strip_prefix("## ") {
            items.push((Kind::Heading, heading.trim().into()));
            open = false;
        } else if let Some(bullet) = line.strip_prefix("- ") {
            items.push((Kind::Bullet, bullet.trim().into()));
            open = true;
        } else if let Some((_, text)) = items.last_mut().filter(|_| open) {
            text.push(' ');
            text.push_str(line);
        } else {
            items.push((Kind::Paragraph, line.into()));
            open = true;
        }
    }
    let line = t.body.line_height();
    let indent = (t.body.size * 1.2).round();
    let mut blocks = Vec::new();
    let mut previous = None;
    for (kind, text) in items {
        if let Some(previous) = previous {
            blocks.push(StyledBlock::Gap(match (previous, kind) {
                (_, Kind::Heading) => line,
                (Kind::Heading, _) | (Kind::Bullet, Kind::Bullet) => line * 0.3,
                _ => line * 0.6,
            }));
        }
        previous = Some(kind);
        let heading = kind == Kind::Heading;
        blocks.push(StyledBlock::Paragraph(StyledParagraph {
            text,
            spans: Vec::new(),
            base: if heading { t.body.weight(Weight::SemiBold) } else { t.body },
            muted: INK2,
            indent: if kind == Kind::Bullet { indent } else { 0.0 },
            inert: true,
            keep: heading,
            bullet: kind == Kind::Bullet,
        }));
    }
    blocks
}

/// The notes in pages at `area` (see `PagedText::place`), or a line
/// saying there are none.
pub fn paged(notes: &str, area: Rect, t: &Theme) -> PagedText {
    let mut blocks = blocks(notes, t);
    if blocks.is_empty() {
        blocks = self::blocks(NO_NOTES, t);
    }
    let mut text = PagedText::new(blocks, "release note");
    text.place(area, t);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (text, heading, bullet) of each paragraph, and the gaps as "gap".
    fn parts(notes: &str) -> Vec<(String, bool, bool)> {
        blocks(notes, &Theme::for_dpi(300))
            .into_iter()
            .map(|b| match b {
                StyledBlock::Paragraph(p) => {
                    assert_eq!(p.indent > 0.0, p.bullet, "bullets, and only bullets, are indented");
                    (p.text, p.keep, p.bullet)
                }
                _ => ("gap".into(), false, false),
            })
            .collect()
    }

    #[test]
    fn notes_read_as_headings_bullets_and_paragraphs() {
        let notes = "Tenth beta.\nStill one paragraph.\n\n## Reading\n- Faster pages,\n  on every Kindle.\n- Fewer flashes.\n\nThanks!\n";
        let p = |s: &str| (s.to_owned(), false, false);
        assert_eq!(
            parts(notes),
            [
                p("Tenth beta. Still one paragraph."),
                p("gap"),
                ("Reading".into(), true, false),
                p("gap"),
                ("Faster pages, on every Kindle.".into(), false, true),
                p("gap"),
                ("Fewer flashes.".into(), false, true),
                p("gap"),
                p("Thanks!"),
            ]
        );
        assert!(parts("").is_empty() && parts("\n  \n").is_empty());
        let t = Theme::for_dpi(300);
        let none = paged("  ", Rect::new(0, 0, 900, 900), &t);
        assert!(matches!(none.blocks(), [StyledBlock::Paragraph(p)] if p.text == NO_NOTES));
    }
}
