#!/usr/bin/env python3
"""Audit curated English base forms against real dictionary headwords.

Usage: scripts/audit-english-forms.py <repo> <dictionary-folder> > audit.tsv
For each WordNet exception form, lists rule guesses (mirroring the rules in
crates/verso/src/dictionary/forms/english.rs) that are real headwords but not curated
lemmas: since curated lemmas are authoritative, these are never shown. A real
alternative belongs in assets/wordforms/english-overrides.txt. Only
dictionaries whose folder names appear in ENGLISH are read; the `core` column
counts hits in Concise Oxford, Macmillan and Cambridge. Keep RULES in sync
with forms/english.rs.
"""
import os, re, sys

repo, dicts = sys.argv[1], sys.argv[2]
src = open(os.path.join(repo, "crates/verso/src/dictionary/forms/english.rs")).read()
no_rules = set(re.findall(r'^\s+"([a-z]+)",$', src.split("const NO_RULES")[1].split("];")[0], re.M))
RULES = [("ies","y"),("ied","y"),("ier","y"),("iest","y"),("s",""),("es",""),("ves","f"),("ves","fe"),
         ("men","man"),("d",""),("ed",""),("ing","e"),("ing",""),("r",""),("er",""),("st",""),("est","")]

def rules(word):
    out = []
    def push(w):
        if len(w) >= 2 and w not in out: out.append(w)
    if word in no_rules: return out
    for suf, rep in RULES:
        if word.endswith(suf):
            stem = word[: len(word) - len(suf)]
            if rep == "y" and len(stem) < 2: continue
            push(stem + rep)
            if rep == "" and suf in ("ed","ing","er","est") and len(stem) >= 2 and stem[-1] == stem[-2]:
                push(stem[:-1])
    return out

ENGLISH = ["Concise Oxford English Dictionary", "Macmillan English Dictionary",
           "Cambridge Advanced Learners Dictionary 3th Ed", "Oxford English Dictionary 2nd Ed",
           "English explanatory dictionary (main)", "wordnet"]
heads = {}
for root, _, files in os.walk(dicts):
    for f in files:
        if f.endswith(".idx") and any(n in root for n in ENGLISH):
            name = next(n for n in ENGLISH if n in root)
            data = open(os.path.join(root, f), "rb").read()
            s, pos = set(), 0
            while pos < len(data):
                end = data.index(b"\0", pos)
                s.add(data[pos:end].decode("utf-8", "replace").lower())
                pos = end + 9
            heads[name] = s
names = [n for n in ENGLISH if n in heads]
short = {"Concise Oxford English Dictionary":"COED","Macmillan English Dictionary":"Mac",
         "Cambridge Advanced Learners Dictionary 3th Ed":"CALD","Oxford English Dictionary 2nd Ed":"OED",
         "English explanatory dictionary (main)":"EngMain","wordnet":"WN"}
core = ["COED","Mac","CALD"]  # learner/concise dictionaries: real modern words
rows = []
for line in open(os.path.join(repo, "assets/wordforms/english-exceptions.txt")):
    form, *lemmas = line.split()
    for cand in rules(form):
        if cand in lemmas: continue
        hits = [short[n] for n in names if cand in heads[n]]
        if not hits: continue
        exact = [short[n] for n in names if form in heads[n]]
        rows.append((sum(h in core for h in hits), form, " ".join(lemmas), cand, ",".join(hits), ",".join(exact)))
rows.sort(key=lambda r: (-r[0], r[1]))
print(f"{len(rows)} lost rule candidates; core-dictionary hits: " +
      ", ".join(f"{k}:{sum(r[0]==k for r in rows)}" for k in (3,2,1,0)))
print("core\tform\tcurated\trule-candidate\theadword-in\tform-exact-in")
for r in rows: print("\t".join(map(str, r)))
