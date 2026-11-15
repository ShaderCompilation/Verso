#!/usr/bin/env python3
"""Merge WordNet 3.0 exceptions; keep the downloaded source outside the repo.

Usage: scripts/gen-english-forms.py /tmp/verso-wordnet/WordNet-3.0/dict
The archive URL and checksum are recorded in docs/dictionary.md.
"""

import argparse
from pathlib import Path


def generate(source: Path) -> str:
    forms: dict[str, list[str]] = {}
    # Preserve source lemma order, then part-of-speech order for ambiguous forms.
    for name in ("noun", "verb", "adj", "adv"):
        for number, line in enumerate((source / f"{name}.exc").read_text(encoding="ascii").splitlines(), 1):
            fields = line.lower().split()
            if len(fields) < 2:
                raise ValueError(f"{name}.exc:{number}: expected a form and at least one lemma")
            form, *lemmas = fields
            # The runtime gate only passes plain letters, so punctuated forms
            # (multi-word `_`, `ad-libbed`, `o'clock`) could never be looked up.
            if not (form.isascii() and form.isalpha()):
                continue
            lemmas = [lemma for lemma in lemmas if "_" not in lemma]
            if not lemmas:
                continue
            merged = forms.setdefault(form, [])
            for lemma in lemmas:
                if lemma not in merged:
                    merged.append(lemma)
    return "".join(f"{form} {' '.join(lemmas)}\n" for form, lemmas in sorted(forms.items()))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wordnet_dict_dir", type=Path)
    args = parser.parse_args()
    # Read everything before writing, so missing/malformed sources cannot truncate the asset.
    data = generate(args.wordnet_dict_dir)
    output = Path(__file__).resolve().parents[1] / "assets/wordforms/english-exceptions.txt"
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(data, encoding="ascii")
    print(f"Wrote {len(data.splitlines())} forms ({len(data)} bytes) to {output}")


if __name__ == "__main__":
    main()
