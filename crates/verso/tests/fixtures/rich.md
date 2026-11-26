---
title: Field Notes on E-Ink Typography
authors:
  - Ada Lovelace
  - Charles Babbage
lang: en
description: Practical notes on layouts, tables, code blocks, task lists, and footnotes in Markdown.
series: Verso Guides
series_index: 1
cover: markdown-cover.png
---

# Field Notes on E-Ink Typography

Paper-like screens reward generous leading, restrained ink, and clear hierarchy.[^1]

This second paragraph verifies that spaced paragraphs stay distinct without relying on first-line indentation, even alongside *italic*, **bold**, ~~deprecated~~, and `inline_code()` spans.

> Good typography disappears so the reader stays inside the prose rather than noticing the frame.[^note]

## Code and Wrapping

```rust
pub fn wrap_measurement_example(very_long_identifier_for_narrow_eink_screens: &str) -> usize {
    very_long_identifier_for_narrow_eink_screens.chars().count()
}
```

## Comparison Table

| Element | Alignment | Behavior |
| :--- | :---: | ---: |
| Paragraph | Left | Spaced |
| Code block | Center | Wrapped |
| Footnote | Right | Sheet |

## Checklist

- [x] Verify monospace code preservation
- [ ] Inspect wrapped table cells on Kindle

## Figure

![Checkerboard sample](markdown-cover.png)

[^1]: First footnote explaining why high-contrast serif type works well at 300 PPI.
[^note]: Second footnote with **formatted** text inside an aside.
