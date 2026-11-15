#!/usr/bin/env python3
"""Original EPUB fixture for manual dictionary and base-form checks.

The book is a set of selectable single words and short passages. It covers
exact lookup, regular and irregular English base forms, possessives, function
words, and inputs that must stay exact-only. No dictionary or book text is
included; every sentence is original and written for this fixture.
"""
from html import escape
from pathlib import Path
import struct
from zipfile import ZipFile, ZipInfo, ZIP_STORED
import zlib

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "crates/verso/tests/fixtures/dictionary-words.epub"


def png_chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def cover_png(width=300, height=400):
    """A small original cover: a cream page with an ink magnifying glass."""
    bg = (38, 59, 82)
    page = (244, 239, 226)
    ink = (38, 59, 82)
    accent = (176, 120, 66)
    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter: none
        for x in range(width):
            inside_page = 40 <= x < width - 40 and 40 <= y < height - 40
            color = page if inside_page else bg
            if inside_page:
                # A title bar and a few lines of text on the page.
                if 62 <= y < 76 and 64 <= x < width - 80:
                    color = ink
                for line_y, line_end in ((108, 224), (128, 200), (148, 232), (250, 210), (270, 238), (290, 184)):
                    if line_y <= y < line_y + 6 and 68 <= x < line_end:
                        color = ink
                dx, dy = x - 151, y - 200
                radius = (dx * dx + dy * dy) ** 0.5
                if radius < 48:
                    color = page
                elif radius <= 57:
                    color = accent
                elif 184 <= x <= 238 and 236 <= y <= 296 and abs((x - 184) - (y - 236)) <= 7:
                    color = accent
            rows.extend(color)
    raw = bytes(rows)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def note(text):
    return f'<p class="note">{escape(text)}</p>'


def word(text, lang=None):
    attr = f' lang="{lang}" xml:lang="{lang}"' if lang else ""
    return f'<p class="word"{attr}>{escape(text)}</p>'


def word_html(html, lang=None):
    attr = f' lang="{lang}" xml:lang="{lang}"' if lang else ""
    return f'<p class="word"{attr}>{html}</p>'


def word_and_note(text, text_note, lang=None):
    return word(text, lang) + note(text_note)


def word_list(words, lang=None):
    return "\n".join(word(w, lang) for w in words)


def page(title, body):
    return f'''<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="en">
<head>
<title>{escape(title)}</title>
<link rel="stylesheet" type="text/css" href="style.css"/>
</head>
<body>
{body}
</body>
</html>'''


how = "\n".join(
    [
        "<h1>Dictionary Word Tests</h1>",
        note("A Verso test fixture. All text is original; it contains no dictionary content."),
        "<h2>How to use</h2>",
        "<ol>",
        "<li>Open this book in Verso. Long-press one bold word and release. The lookup sheet opens.</li>",
        "<li>Read the note above a group before testing; it says what the group exercises.</li>",
        "<li>Use a definitions-only English dictionary first, such as a WordNet gloss dictionary. Then add Macmillan or Concise Oxford and repeat.</li>",
        "<li>Exact entries always win. A word can show a base-form line with one dictionary set and no line with another.</li>",
        "<li>Long-press and drag across several words to test a passage. Passage queries never get base forms.</li>",
        "<li>Tap the dictionary name in the lookup sheet to switch dictionaries and compare.</li>",
        "</ol>",
        "<h2>What is tested</h2>",
        "<ul>",
        "<li>Exact lookup before guessed English base forms, across dictionaries in preference order.</li>",
        "<li>Regular plurals, past tense, -ing forms and comparisons.</li>",
        "<li>WordNet irregular exception lists and the curated be/do supplement.</li>",
        "<li>Straight and curly possessive apostrophes.</li>",
        "<li>Common function words that skip suffix rules, and valid reductions that stay enabled.</li>",
        "<li>Inputs that are exact-only: accents, Cyrillic, passages, digits, hyphens, punctuation and over-long words.</li>",
        "<li>Selection across inline markup, punctuation, soft hyphens and ligatures.</li>",
        "</ul>",
        note("Base forms are guesses checked against real headwords. A match does not prove the guess is semantically correct; the lookup sheet says only that the base form was found."),
        note("Generated by scripts/gen-dictionary-fixture.py. Edit the script, not the EPUB."),
    ]
)

regular_1 = "\n".join(
    [
        "<h1>Regular forms: plurals and past tense</h1>",
        note("Each word below should find a base form when no enabled dictionary has the inflected word. Exact entries win, so use a definitions-only dictionary to see the fallback."),
        "<h2>Straightforward plurals</h2>",
        note("Expected headwords: thunder, apple, box, wolf, knife, wife, half, tree, office, bus, glass, church, berry, baby, key, day, toy, hero, potato, photo."),
        word_list(
            [
                "thunders",
                "apples",
                "boxes",
                "wolves",
                "knives",
                "wives",
                "halves",
                "trees",
                "offices",
                "buses",
                "glasses",
                "churches",
                "berries",
                "babies",
                "keys",
                "days",
                "toys",
                "heroes",
                "potatoes",
                "photos",
            ]
        ),
        "<h2>Plurals with a choice</h2>",
        word_and_note("studies", "Base form: study. Oxford and Macmillan can have an exact studies entry, which wins."),
        word_and_note("uses", "Base form: use. The -s rule is tried before -es, so the result should never be us."),
        word_and_note("leaves", "Base forms: leaf and leave. Both are exception lemmas; one or both can appear."),
        word_and_note("lives", "Base form: life from the exception list, then live from the -s rule. Both can appear."),
        word_and_note("news", "News is a base noun. It must never reduce to new."),
        "<h2>Past tense and -ed forms</h2>",
        note("Useful checks: hoped → hope (not hop), used → use (not us), kissed → kiss (not kis), planned → plan, stopped → stop, tried → try, died → die, carried → carry, travelled → travel, preferred → prefer, created → create."),
        note("Played and enjoyed check that the -y rules do not fire after a vowel."),
        word_list(
            [
                "studied",
                "hoped",
                "used",
                "kissed",
                "planned",
                "stopped",
                "carried",
                "tried",
                "died",
                "lied",
                "moved",
                "loved",
                "cared",
                "created",
                "travelled",
                "preferred",
                "visited",
                "opened",
                "asked",
                "cleaned",
                "played",
                "enjoyed",
            ]
        ),
        "<h2>In context</h2>",
        '<p class="sentence">The wolves were running while she studied; the leaves fell and the children played.</p>',
        note("Select one word at a time, then select the whole sentence as a passage. Only the single words get base forms."),
    ]
)

regular_2 = "\n".join(
    [
        "<h1>Regular forms: -ing and comparisons</h1>",
        note("The expected base forms are listed above each group. Harder cases are called out individually."),
        "<h2>-ing forms</h2>",
        note("Expected: hope, use, care, run, plan, stop, kiss, study, move, love, create, get, sit, swim, begin, travel, prefer, visit, open, ask, play, enjoy, be, do."),
        note("hoping → hope (not hop), using → use (not us), caring → care (not car), running → run (not runn), planning → plan, stopping → stop, kissing → kiss, studying → study, getting → get, sitting → sit, swimming → swim, beginning → begin, travelling → travel, preferring → prefer."),
        note("Being and doing are the curated supplement, so they find be and do before any suffix rule."),
        word_list(
            [
                "hoping",
                "using",
                "caring",
                "running",
                "planning",
                "stopping",
                "kissing",
                "studying",
                "moving",
                "loving",
                "creating",
                "getting",
                "sitting",
                "swimming",
                "beginning",
                "travelling",
                "preferring",
                "visiting",
                "opening",
                "asking",
                "playing",
                "enjoying",
                "being",
                "doing",
            ]
        ),
        "<h2>Comparatives and superlatives</h2>",
        note("Expected: happy, happy, nice, nice, big, big, sad, sad, late, late, fine, fine, hot, hot, simple, simple, early, early."),
        note("Bigger and biggest must not stop at bigg. Later and latest can also be exact entries in a dictionary."),
        word_list(
            [
                "happier",
                "happiest",
                "nicer",
                "nicest",
                "bigger",
                "biggest",
                "sadder",
                "saddest",
                "later",
                "latest",
                "finer",
                "finest",
                "hotter",
                "hottest",
                "simpler",
                "simplest",
                "earlier",
                "earliest",
            ]
        ),
        "<h2>In context</h2>",
        '<p class="sentence">She was hoping, planning and running while the happier children played.</p>',
        note("Select hoping, planning, running or happier, then select the whole sentence to compare the single-word path with a passage."),
    ]
)

irregular = "\n".join(
    [
        "<h1>Irregular forms</h1>",
        note("Irregular forms use WordNet's exception lists. Some real dictionaries also have exact entries for these forms; exact wins and no base-form line appears. Use a definitions-only dictionary to see the exception behavior."),
        "<h2>Irregular verbs</h2>",
        note("went, gone → go; was, were, been, being → be; has, had → have; does, did, done → do; knew, known → know; took, taken → take; saw, seen → see; made → make; said → say; found → find; thought → think; brought → bring; bought → buy; caught → catch; taught → teach; felt → feel; left → leave; kept → keep; slept → sleep; ran → run."),
        note("ate, eaten → eat; wrote, written → write; spoke, spoken → speak; broke, broken → break; chose, chosen → choose; drove, driven → drive; rose, risen → rise; fell, fallen → fall; forgot, forgotten → forget; began, begun → begin; drank, drunk → drink; sang, sung → sing; swam, swum → swim."),
        note("Saw and left are also ordinary base words (a tool and a direction). Exact entries win."),
        word_list(
            [
                "went",
                "gone",
                "was",
                "were",
                "been",
                "being",
                "has",
                "had",
                "does",
                "did",
                "done",
                "knew",
                "known",
                "took",
                "taken",
                "saw",
                "seen",
                "made",
                "said",
                "found",
                "thought",
                "brought",
                "bought",
                "caught",
                "taught",
                "felt",
                "left",
                "kept",
                "slept",
                "ran",
                "ate",
                "eaten",
                "wrote",
                "written",
                "spoke",
                "spoken",
                "broke",
                "broken",
                "chose",
                "chosen",
                "drove",
                "driven",
                "rose",
                "risen",
                "fell",
                "fallen",
                "forgot",
                "forgotten",
                "began",
                "begun",
                "drank",
                "drunk",
                "sang",
                "sung",
                "swam",
                "swum",
            ]
        ),
        "<h2>Irregular plurals</h2>",
        note("Expected: child, man, woman, foot, tooth, mouse, goose, ox, brother, penny, louse, alumnus, cactus, fungus, nucleus, syllabus, vortex, crisis, thesis, index, matrix, criterion, phenomenon, datum, medium."),
        note("Women → woman uses the men→man rule rather than an exception. Data and media can also be exact entries."),
        word_list(
            [
                "children",
                "men",
                "women",
                "feet",
                "teeth",
                "mice",
                "geese",
                "oxen",
                "brethren",
                "pence",
                "lice",
                "alumni",
                "cacti",
                "fungi",
                "nuclei",
                "syllabi",
                "vortices",
                "crises",
                "theses",
                "indices",
                "matrices",
                "criteria",
                "phenomena",
                "data",
                "media",
            ]
        ),
        "<h2>Comparisons and no-guess words</h2>",
        note("better and best → good + well; best can also add be through the st→ rule, up to the three-form cap. worse and worst → bad. farther and further → far. less, least and most have no rule guesses, and more has no matching suffix rule; these four are exact-only in practice."),
        word_list(["better", "best", "worse", "worst", "farther", "further", "less", "least", "more", "most"]),
        "<h2>In context</h2>",
        '<p class="sentence">The children went home after the geese and mice ran across the field.</p>',
        note("Select one word, then drag across the sentence. The passage should not produce base forms."),
    ]
)

possessives = "\n".join(
    [
        "<h1>Possessives</h1>",
        note("A straight or curly possessive is stripped first, then looked up like an exact word. If that word is missing, its own base forms are tried."),
        "<h2>Singular possessives</h2>",
        word_and_note("John's", "Base form: john. The result should be labelled Base form of “John's”."),
        word_and_note("John’s", "The curly apostrophe should behave exactly like the straight one."),
        word_and_note("children's", "Base form: child, reached through the irregular children entry."),
        word_and_note("children’s", "Curly apostrophe control for the same path."),
        word_and_note("reader's", "Base form: reader."),
        word_and_note("reader’s", "Curly apostrophe control."),
        word_and_note("writer's", "Base form: writer."),
        word_and_note("writer’s", "Curly apostrophe control."),
        word_and_note("dog's", "Base form: dog."),
        word_and_note("dog’s", "Curly apostrophe control."),
        word_and_note("James's", "Base form: james."),
        word_and_note("James’s", "Curly apostrophe control."),
        word_and_note("James'", "Only the apostrophe is removed, then james is looked up."),
        word_and_note("it's", "Not a possessive in meaning, but the lookup strips 's and can show it as a base form of it when it exists."),
        word_and_note("it’s", "Curly apostrophe control for the same limitation."),
        word_and_note("everyone's", "Base form: everyone, when that headword exists."),
        word_and_note("everyone’s", "Curly apostrophe control."),
        "<h2>Plural possessives</h2>",
        word_and_note("dogs'", "Only the apostrophe is removed, leaving dogs; the -s rule then finds dog."),
        word_and_note("dogs’", "Curly apostrophe control for the plural possessive."),
        "<h2>Punctuated possessives</h2>",
        word_and_note("mother-in-law's", "Hyphens remain after stripping, so this is exact-only; no base form."),
        word_and_note("O'Brien's", "The remaining apostrophe keeps this out of the base-form gate; exact-only."),
        word_and_note("rock'n'roll's", "Remaining apostrophes make this exact-only."),
        "<h2>In context</h2>",
        '<p class="sentence">John\'s dog\'s bowl is beside the children\'s books.</p>',
        note("Select John's, dog's or children's on its own, then drag across the whole sentence."),
    ]
)

exact = "\n".join(
    [
        "<h1>Exact entries and tricky guesses</h1>",
        note("These words expose precedence and candidate quality. Exact entries always win, so the same word can behave differently with different dictionary sets. Switch dictionaries in the lookup sheet to compare."),
        "<h2>Exact entries win</h2>",
        word_and_note("news", "Exact noun in most dictionaries. It must never reduce to new."),
        word_and_note("studies", "Exact in Oxford and Macmillan. A WordNet gloss dictionary lacks it, so it falls back to study."),
        word_and_note("went", "Exact in Macmillan and the dict.org WordNet edition. A WordNet gloss dictionary falls back to go."),
        word_and_note("axes", "Exact in Oxford. Otherwise the exception list gives ax and axis."),
        word_and_note("best", "Exact in the checked WordNet, Macmillan and Oxford dictionaries. Without an exact entry, expect good and well, and possibly be from the st→ rule."),
        word_and_note("lying", "Exact in the checked dictionaries. With the English explanatory dictionary (main), which lacks lie but has lye, the rule-only guess lye can appear. This is a known limitation."),
        word_and_note("thunder", "Exact control. No base-form line should appear."),
        "<h2>Guesses that need a closer look</h2>",
        word_and_note("uses", "use only. The -s rule is tried before -es, so us should not appear."),
        word_and_note("hoping", "hope only. The e-restoring rule comes before plain -ing stripping, so hop should not appear."),
        word_and_note("caring", "care only, not car."),
        word_and_note("using", "use only, not us."),
        word_and_note("dying", "die from the exception list. If dye also exists, it can be added as the first rule hit."),
        word_and_note("leaves", "leaf and leave are both exception lemmas; both can appear when leaves itself is missing."),
        word_and_note("lives", "life from the exception list; live can be added from the -s rule."),
        word_and_note("better", "good and well from the exception list."),
        word_and_note("worse", "bad from the exception list."),
        word_and_note("women", "woman through the men→man rule, not an exception entry."),
        word_and_note("children", "child from the exception list."),
        word_and_note("men", "man from the exception list."),
        "<h2>In context</h2>",
        '<p class="sentence">She studies the news, hoping the best is not behind her.</p>',
        note("Try this sentence with a WordNet gloss dictionary, then with Macmillan or Oxford, and compare which lines appear."),
    ]
)

function_words = "\n".join(
    [
        "<h1>Function words and no-guess words</h1>",
        note("These words should not produce suffix guesses. A dictionary with an exact entry still shows it; otherwise expect No result, without a base-form line."),
        "<h2>No suffix rules</h2>",
        note("The verified pair is his and against. Against must not become again. The other words extend the same closed-class list."),
        word_list(
            [
                "his",
                "her",
                "and",
                "nor",
                "yes",
                "against",
                "must",
                "this",
                "always",
                "perhaps",
                "whereas",
                "almost",
                "never",
                "nevertheless",
                "nonetheless",
                "nothing",
                "should",
                "could",
                "would",
                "another",
                "anything",
                "everything",
                "something",
                "whatever",
                "whenever",
                "wherever",
                "whether",
                "whilst",
                "toward",
                "under",
                "unless",
                "versus",
            ]
        ),
        "<h2>Curated exceptions still apply</h2>",
        note("These words skip suffix rules but keep their exception lemmas."),
        word_and_note("was", "Base form: be."),
        word_and_note("has", "Base form: have."),
        word_and_note("does", "Base form: do, from the curated supplement."),
        "<h2>Valid reductions that stay enabled</h2>",
        note("These are not blocked: their guesses are valid base words. Expected: her, our, your, their, toward, afterward, forward, upward."),
        word_list(["hers", "ours", "yours", "theirs", "towards", "afterwards", "forwards", "upwards"]),
        "<h2>In context</h2>",
        '<p class="sentence">She walked towards the sea and afterwards told him everything.</p>',
        note("Towards and afterwards should reduce, everything should not, and him depends on whether the dictionary has an exact entry."),
    ]
)

overlong = "antidisestablishmentarianism" * 3

rejected = "\n".join(
    [
        "<h1>Exact-only inputs</h1>",
        note("Base forms are only attempted for one ASCII-letter word of at most 64 bytes, optionally with a possessive ending. Everything here should be exact-only or No result."),
        "<h2>Accents and non-Latin scripts</h2>",
        note("An exact entry such as café or привет is still valid. The point is that no base-form line appears."),
        word_list(["café", "cafe", "naïve", "résumé", "Zürich", "Straße"]),
        word_html("cafe\u0301", lang="en"),
        note("The second cafe uses a combining acute accent; it is not plain ASCII."),
        word("привет", "ru"),
        word("приветы", "ru"),
        word("ПРИВЕТ", "ru"),
        word("мир", "ru"),
        note("привет has an exact Russian entry. приветы is a miss and must not be reduced by English rules."),
        "<h2>Passages</h2>",
        note("Select each whole phrase by long-pressing and dragging. Passage queries never get base forms."),
        word_list(["running water", "thunders and", "tilting at windmills"]),
        word_html(f"New&nbsp;York"),
        note("The last phrase uses a non-breaking space."),
        "<h2>Digits, hyphens and apostrophes</h2>",
        note("These contain characters that the base-form gate does not accept after possessive stripping."),
        word_list(["MP3s", "2020s", "3D", "B2B", "e-mail", "mother-in-law", "rock'n'roll"]),
        word_and_note("O'Brien's", "The apostrophe remains after possessive stripping, so this is exact-only."),
        word_and_note("mother-in-law's", "The hyphens remain, so this is exact-only."),
        word_and_note("s'", "A bare s' is not a possessive and is rejected."),
        "<h2>Single letters and over-long input</h2>",
        note("Rule outputs shorter than two letters are dropped. The long word is 84 bytes, above the 64-byte gate."),
        word_list(["a", "I"]),
        word(overlong),
        note(f"The long word has {len(overlong)} ASCII letters: exact lookup only."),
        "<h2>In context</h2>",
        '<p class="sentence">They were running water and reading about café life in Zürich.</p>',
        note("Select one word, then the whole phrase, to compare exact-only and passage behavior."),
    ]
)

characters = "\n".join(
    [
        "<h1>Characters, case and markup</h1>",
        note("These checks show how the selected text reaches the lookup. The word-form layer only sees what the selection provides."),
        "<h2>Case</h2>",
        note("The dictionary index is case-insensitive, so all three should find the same base form."),
        word_list(["thunders", "Thunders", "THUNDERS"]),
        "<h2>Punctuation around the word</h2>",
        note("If the punctuation is included in the selection, the lookup can miss. That shows what the selection layer passed through; it is not a word-form failure."),
        word_html("“thunders”"),
        word_html("(thunders)"),
        word_html("thunders,"),
        word_html("thunders."),
        word_html("—thunders—"),
        word_html("thunders…"),
        "<h2>Inline markup</h2>",
        note("Select the whole rendered word. If crengine returns the full word, base forms work; if selection stops at the markup boundary, that limitation is visible."),
        word_html("thun<em>ders</em>"),
        word_html("hope<strong>ing</strong>"),
        word_html("run<em>ning</em>"),
        word_html("kiss<em>ed</em>"),
        "<h2>Soft hyphens and ligatures</h2>",
        note("A soft hyphen is not an ASCII letter. thunders with a soft hyphen should ideally behave like plain thunders; extra­ordinary has no base form either way."),
        word_html("thun&#173;ders"),
        word_html("extra&#173;ordinary"),
        word_html("o\ufb03ce"),
        note("The last word uses the ﬃ ligature. It is not plain ASCII, so there is no base-form pass. Exact lookup shows whether the ligature is normalized."),
        "<h2>Spacing</h2>",
        note("New York is a passage, whether it uses a normal space or a non-breaking space."),
        word_html("New York"),
        word_html("New&nbsp;York"),
        "<h2>In context</h2>",
        '<p class="sentence">The thun<em>ders</em> rolled and she hoped to read o&#64257;ce reports in Zürich.</p>',
        note("Select thunders across the markup, then hoped, then the whole sentence."),
    ]
)

regression = "\n".join(
    [
        "<h1>Quick regression set</h1>",
        note("The 2026-11-15 base-form verification queries in one place. Use a WordNet gloss dictionary alone first, then add Macmillan and Concise Oxford."),
        word_and_note("thunders", "thunder with a singular base-form line."),
        word_and_note("studies", "study with WordNet alone; Oxford's exact studies hides the line."),
        word_and_note("went", "go with WordNet alone; Macmillan's exact went hides the line."),
        word_and_note("hoping", "hope, never hop."),
        word_and_note("axes", "ax and axis with WordNet alone; Oxford's exact axes hides the line."),
        word_and_note("John's", "john, one base form and a singular line."),
        word_and_note("his", "No result and no base-form line in the WordNet gloss dictionary."),
        word_and_note("against", "No result; must not become again."),
        word_and_note("thunder", "Exact control with no extra line."),
        word("привет", "ru"),
        note("Exact Russian entry, no extra line."),
        word("приветы", "ru"),
        note("Russian miss; no English reduction."),
        word_and_note("best", "Exact in the checked dictionaries; without it, good and well, possibly with be."),
        word_and_note("lying", "Exact in the checked dictionaries; lye is a known rule-only false positive with the English explanatory dictionary (main)."),
        note("See docs/dictionary.md for the full verification notes and timings."),
    ]
)

CHAPTERS = [
    ("how", "How to use", how),
    ("regular", "Regular forms: plurals and past", regular_1),
    ("ing", "Regular forms: -ing and comparisons", regular_2),
    ("irregular", "Irregular forms", irregular),
    ("possessives", "Possessives", possessives),
    ("exact", "Exact entries and tricky guesses", exact),
    ("function", "Function words and no-guess words", function_words),
    ("rejected", "Exact-only inputs", rejected),
    ("characters", "Characters, case and markup", characters),
    ("regression", "Quick regression set", regression),
]

style = """body { margin: 0 1.2em; line-height: 1.45; }
h1 { font-size: 1.45em; margin: 0 0 1em; }
h2 { font-size: 1.1em; margin: 1.6em 0 0.5em; }
p { margin: 0.55em 0; }
p.note { font-size: 0.86em; font-style: italic; margin: 0.4em 0 0.8em; }
p.word { font-size: 1.15em; font-weight: bold; margin: 0.65em 0; }
p.sentence { margin: 1em 0; }
"""

manifest_items = "\n".join(
    f'<item id="{cid}" href="{cid}.xhtml" media-type="application/xhtml+xml"/>' for cid, _, _ in CHAPTERS
)
spine_items = "\n".join(f'<itemref idref="{cid}"/>' for cid, _, _ in CHAPTERS)
nav_points = "\n".join(
    f'<navPoint id="nav-{cid}" playOrder="{i}"><navLabel><text>{escape(title)}</text></navLabel>'
    f'<content src="{cid}.xhtml"/></navPoint>'
    for i, (cid, title, _) in enumerate(CHAPTERS, 1)
)

book_opf = f'''<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:identifier id="id">verso-dictionary-words</dc:identifier>
<dc:title>Dictionary Word Tests</dc:title>
<dc:creator>Verso</dc:creator>
<dc:language>en</dc:language>
<meta name="cover" content="cover"/>
</metadata>
<manifest>
<item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
<item id="style" href="style.css" media-type="text/css"/>
<item id="cover" href="cover.png" media-type="image/png"/>
{manifest_items}
</manifest>
<spine toc="ncx">
{spine_items}
</spine>
</package>'''

toc_ncx = f'''<?xml version="1.0" encoding="utf-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
<head><meta name="dtb:uid" content="verso-dictionary-words"/></head>
<docTitle><text>Dictionary Word Tests</text></docTitle>
<navMap>
{nav_points}
</navMap>
</ncx>'''

container = '''<?xml version="1.0" encoding="utf-8"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
<rootfiles><rootfile full-path="book.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>'''

OUT.parent.mkdir(parents=True, exist_ok=True)
with ZipFile(OUT, "w", ZIP_STORED) as epub:
    def add(name, data):
        info = ZipInfo(name, (2026, 1, 1, 0, 0, 0))
        info.external_attr = 0o644 << 16
        epub.writestr(info, data)

    add("mimetype", "application/epub+zip")
    add("META-INF/container.xml", container)
    add("book.opf", book_opf)
    add("toc.ncx", toc_ncx)
    add("style.css", style)
    add("cover.png", cover_png())
    for cid, title, body in CHAPTERS:
        add(f"{cid}.xhtml", page(title, body))

assert len(overlong) > 64
print(OUT)
