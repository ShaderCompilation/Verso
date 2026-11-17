# OpenCorpora morphological dictionary (kept source)

`dict.opcorpora.xml.bz2` is the OpenCorpora morphological dictionary export,
the source of Verso's Russian word-form pack (`ru.vwf`). opencorpora.org no
longer serves it (a bare nginx page on 2026-11-17, the export URL returns
404), so this copy is the only reproducible source. Build the pack with
`scripts/build-wordforms.sh`; see
[docs/word-forms-languages.md](../../../docs/word-forms-languages.md).

| | |
|---|---|
| Original URL | `https://www.opencorpora.org/files/export/dict/dict.opcorpora.xml.bz2` |
| Downloaded from | <https://web.archive.org/web/20240423122538id_/https://www.opencorpora.org/files/export/dict/dict.opcorpora.xml.bz2> (Internet Archive capture of 2024-04-23 12:25:38 UTC), on 2026-11-17 |
| Root element | `<dictionary version="0.92" revision="417258">` |
| `dict.opcorpora.xml.bz2` | 16,962,307 bytes, SHA-256 `f27150955c5f0978ece14348096f7d1380fc3564b08fdf99f129ede586309b6c` |
| Decompressed XML | 420,742,862 bytes, SHA-256 `eff155b0a5244f3cd84eed19de87abf2aec47d90011177d9847e83882adbc589` |
| Contents | 391,843 lemmas, 5,141,279 word forms, 258,650 links between lemmas (27 link types) |
| Licence | CC BY-SA 3.0, as stated on the OpenCorpora downloads page ("Морфологический словарь … CC BY-SA 3.0"); full text in `LICENSE-CC-BY-SA-3.0.txt` |

Check the copy with `sha256sum dict.opcorpora.xml.bz2`.

The Internet Archive also has a later capture, a `.zip` from 2025-05-06
(revision 417260, 28,757,238 bytes). It is two revisions newer and its XML is
1,083 bytes longer, so it was not worth switching.

## Licence and attribution

The dictionary is © the OpenCorpora project (<https://opencorpora.org>),
licensed under the [Creative Commons Attribution-ShareAlike 3.0 Unported
licence](https://creativecommons.org/licenses/by-sa/3.0/).
`LICENSE-CC-BY-SA-3.0.txt` is the licence's legal code, fetched from
<https://creativecommons.org/licenses/by-sa/3.0/legalcode.txt> on 2026-11-17.

`ru.vwf` is an adaptation of this dictionary (forms folded, lemmas linked and
re-encoded) and is distributed under the same licence: wherever the pack is
installed, this licence text goes with it, with this attribution.
