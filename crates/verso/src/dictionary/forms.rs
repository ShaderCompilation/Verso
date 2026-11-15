//! Bounded English base-form guesses. Callers must check real dictionary
//! headwords; none of these transformations proves that a word is English.

const EXCEPTIONS: &str = include_str!("../../../../assets/wordforms/english-exceptions.txt");
const MAX_QUERY_BYTES: usize = 64;
/// Common forms WordNet doesn't list, where the rules alone would pick the
/// wrong word (`does → doe`, `doing → doe`, `being → bee`). Sorted; no form
/// here may also be in WordNet's list.
const SUPPLEMENT: &[(&str, &[&str])] = &[("being", &["be"]), ("does", &["do"]), ("doing", &["do"])];

/// Function words and fixed adverbs whose suffixes give misleading guesses.
/// Sorted for binary search; possessives and curated exceptions still apply.
const NO_RULES: &[&str] = &[
    "aboard",
    "across",
    "after",
    "afterward",
    "against",
    "ahead",
    "almost",
    "altogether",
    "always",
    "amid",
    "and",
    "another",
    "anything",
    "around",
    "backward",
    "beforehand",
    "behind",
    "being",
    "beyond",
    "could",
    "did",
    "does",
    "doing",
    "downward",
    "during",
    "either",
    "ever",
    "everything",
    "far",
    "farther",
    "farthest",
    "first",
    "for",
    "forever",
    "forward",
    "further",
    "furthest",
    "had",
    "has",
    "henceforward",
    "her",
    "hereafter",
    "his",
    "hither",
    "however",
    "indeed",
    "instead",
    "inward",
    "just",
    "last",
    "least",
    "less",
    "lest",
    "minus",
    "moreover",
    "most",
    "must",
    "near",
    "need",
    "neither",
    "never",
    "nevertheless",
    "nonetheless",
    "nor",
    "nothing",
    "notwithstanding",
    "other",
    "our",
    "outward",
    "over",
    "past",
    "per",
    "perhaps",
    "plus",
    "rather",
    "round",
    "should",
    "something",
    "their",
    "thereafter",
    "this",
    "thither",
    "thus",
    "together",
    "toward",
    "under",
    "unless",
    "upward",
    "versus",
    "was",
    "whatever",
    "whatsoever",
    "whenever",
    "whereafter",
    "whereas",
    "wherever",
    "whether",
    "whichever",
    "whichsoever",
    "whilst",
    "whither",
    "whoever",
    "whomever",
    "whomsoever",
    "whosoever",
    "would",
    "yes",
    "yonder",
    "your",
];

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Candidates {
    /// Try this as an exact word before exceptions or rules.
    pub possessive: Option<String>,
    /// All curated lemmas, in source order.
    pub exceptions: Vec<&'static str>,
    /// Ordered, unique guesses; only the first rule with an entry counts,
    /// after collecting exception hits.
    pub rules: Vec<String>,
}

/// Accept only a single ASCII word, allowing a straight or curly possessive
/// ending. The byte limit applies to the original query, before stripping.
pub(super) fn candidates(query: &str) -> Option<Candidates> {
    if query.is_empty() || query.len() > MAX_QUERY_BYTES {
        return None;
    }
    let lower = query.to_ascii_lowercase();
    let stripped = lower
        .strip_suffix("'s")
        .or_else(|| lower.strip_suffix("’s"))
        // A plural possessive needs a word before its `s`: a bare `s'` is not one.
        .or_else(|| lower.strip_suffix("s'").filter(|before| !before.is_empty()).map(|_| &lower[..lower.len() - 1]))
        .or_else(|| {
            lower.strip_suffix("s’").filter(|before| !before.is_empty()).map(|_| &lower[..lower.len() - '’'.len_utf8()])
        });
    let word = stripped.unwrap_or(&lower);
    if word.is_empty() || !word.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }

    let mut rules = Vec::new();
    if NO_RULES.binary_search(&word).is_err() {
        for (suffix, replacement) in [
            ("ies", "y"),
            ("ied", "y"),
            ("ier", "y"),
            ("iest", "y"),
            ("s", ""),
            ("es", ""),
            ("ves", "f"),
            ("ves", "fe"),
            ("men", "man"),
            ("d", ""),
            ("ed", ""),
            ("ing", "e"),
            ("ing", ""),
            ("r", ""),
            ("er", ""),
            ("st", ""),
            ("est", ""),
        ] {
            if let Some(stem) = word.strip_suffix(suffix) {
                // A one-letter stem makes a two-letter `y` word that is usually an
                // abbreviation (dies → dy, lies → ly); the `s`/`d` rules find die/lie.
                if replacement == "y" && stem.len() < 2 {
                    continue;
                }
                push_rule(&mut rules, format!("{stem}{replacement}"));
                // Preserve the intact stem first: kissed -> kiss, never just kis.
                if replacement.is_empty() && matches!(suffix, "ed" | "ing" | "er" | "est") {
                    let bytes = stem.as_bytes();
                    if bytes.len() >= 2 && bytes[bytes.len() - 1] == bytes[bytes.len() - 2] {
                        push_rule(&mut rules, stem[..stem.len() - 1].to_owned());
                    }
                }
            }
        }
    }
    let exceptions = match exception_lemmas(EXCEPTIONS, word) {
        Some(lemmas) => lemmas.split(' ').collect(),
        None => SUPPLEMENT.iter().find(|(form, _)| *form == word).map_or_else(Vec::new, |(_, lemmas)| lemmas.to_vec()),
    };
    Some(Candidates { possessive: stripped.map(str::to_owned), exceptions, rules })
}

fn push_rule(rules: &mut Vec<String>, word: String) {
    if word.len() >= 2 && !rules.contains(&word) {
        rules.push(word);
    }
}

/// Binary search the sorted ASCII text without building a line/word table or
/// allocating. Both bounds are line starts (or EOF); snap the midpoint back
/// to the start of its line, then discard that whole line on each comparison.
fn exception_lemmas<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let (mut low, mut high) = (0, text.len());
    while low < high {
        let mid = low + (high - low) / 2;
        let start = low + text[low..mid].rfind('\n').map_or(0, |i| i + 1);
        let end = start + text[start..].find('\n').unwrap_or(text.len() - start);
        let (form, lemmas) = text[start..end].split_once(' ')?;
        match form.cmp(word) {
            std::cmp::Ordering::Less => low = (end + 1).min(text.len()),
            std::cmp::Ordering::Greater => high = start,
            std::cmp::Ordering::Equal => return Some(lemmas),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_words_skip_rules_after_normalization() {
        for word in [
            "his", "her", "and", "nor", "yes", "against", "this", "must", "always", "perhaps", "whereas", "HIS", "ThIs",
        ] {
            assert!(candidates(word).unwrap().rules.is_empty(), "{word}");
        }
        for word in ["other's", "other’s", "OTHER'S", "OTHER’S"] {
            let forms = candidates(word).unwrap();
            assert_eq!(forms.possessive.as_deref(), Some("other"));
            assert!(forms.rules.is_empty(), "{word}");
        }
    }

    #[test]
    fn function_words_keep_exception_lemmas() {
        for (word, expected) in [
            ("was", "be"),
            ("has", "have"),
            ("does", "do"),
            ("had", "have"),
            ("did", "do"),
            ("being", "be"),
            ("doing", "do"),
        ] {
            let forms = candidates(word).unwrap();
            assert_eq!(forms.exceptions, [expected], "{word}");
            assert!(forms.rules.is_empty(), "{word}");
        }
    }

    #[test]
    fn valid_function_word_reductions_keep_rules() {
        for (word, expected) in [
            ("hers", "her"),
            ("towards", "toward"),
            ("others", "other"),
            ("theirs", "their"),
            ("ours", "our"),
            ("yours", "your"),
            ("its", "it"),
        ] {
            assert_eq!(candidates(word).unwrap().rules, [expected], "{word}");
        }
    }

    #[test]
    fn no_rules_is_sorted_lowercase_unique_and_suppresses_rules() {
        for pair in NO_RULES.windows(2) {
            assert!(pair[0] < pair[1], "unsorted or duplicate: {}", pair[1]);
        }
        for word in NO_RULES {
            assert!(!word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase()), "{word}");
            assert!(candidates(word).unwrap().rules.is_empty(), "{word}");
        }
    }

    #[test]
    fn regular_forms_keep_rule_priority() {
        for (word, expected) in [
            ("thunders", vec!["thunder"]),
            ("studies", vec!["study", "studie", "studi"]),
            ("studied", vec!["study", "studie", "studi"]),
            ("happiest", vec!["happy", "happie", "happi"]),
            ("boxes", vec!["boxe", "box"]),
            ("wolves", vec!["wolve", "wolv", "wolf", "wolfe"]),
            ("women", vec!["woman"]),
            ("happier", vec!["happy", "happie", "happi"]),
        ] {
            assert_eq!(candidates(word).unwrap().rules, expected, "{word}");
        }
    }

    #[test]
    fn y_rules_need_a_stem_of_two_letters() {
        for (word, expected) in [
            ("dies", vec!["die", "di"]),
            ("died", vec!["die", "di"]),
            ("lies", vec!["lie", "li"]),
            ("ties", vec!["tie", "ti"]),
            ("pier", vec!["pie", "pi"]),
            ("spies", vec!["spy", "spie", "spi"]),
            ("dried", vec!["dry", "drie", "dri"]),
        ] {
            assert_eq!(candidates(word).unwrap().rules, expected, "{word}");
        }
    }

    #[test]
    fn restored_e_precedes_bare_stripping() {
        for (word, expected) in [
            ("hoping", vec!["hope", "hop"]),
            ("used", vec!["use", "us"]),
            ("nicer", vec!["nice", "nic"]),
            ("hoped", vec!["hope", "hop"]),
        ] {
            assert_eq!(candidates(word).unwrap().rules, expected, "{word}");
        }
    }

    #[test]
    fn repeated_letters_keep_intact_stem_first() {
        for (word, expected) in [
            ("running", vec!["runne", "runn", "run"]),
            ("planned", vec!["planne", "plann", "plan"]),
            ("bigger", vec!["bigge", "bigg", "big"]),
            ("biggest", vec!["bigge", "bigg", "big"]),
            ("kissed", vec!["kisse", "kiss", "kis"]),
            ("trees", vec!["tree", "tre"]),
            ("balls", vec!["ball"]),
        ] {
            assert_eq!(candidates(word).unwrap().rules, expected, "{word}");
        }
    }

    #[test]
    fn irregulars_keep_all_lemmas_in_source_order() {
        for (word, expected) in [
            ("went", vec!["go"]),
            ("knelt", vec!["kneel"]),
            ("axes", vec!["ax", "axis"]),
            ("dying", vec!["die"]),
            // Supplement entries, missing from WordNet.
            ("does", vec!["do"]),
            ("DOING", vec!["do"]),
            ("being", vec!["be"]),
        ] {
            assert_eq!(candidates(word).unwrap().exceptions, expected, "{word}");
        }
    }

    #[test]
    fn supplement_is_sorted_and_adds_only_forms_wordnet_lacks() {
        for pair in SUPPLEMENT.windows(2) {
            assert!(pair[0].0 < pair[1].0, "unsorted or duplicate: {}", pair[1].0);
        }
        for (form, lemmas) in SUPPLEMENT {
            assert!(form.bytes().all(|b| b.is_ascii_lowercase()), "{form}");
            assert!(!lemmas.is_empty(), "{form}");
            assert_eq!(exception_lemmas(EXCEPTIONS, form), None, "{form} is now in WordNet's list");
        }
    }

    #[test]
    fn possessives_strip_then_generate_from_the_remaining_word() {
        for word in ["John's", "John’s", "JOHN'S", "JOHN’S"] {
            let forms = candidates(word).unwrap();
            assert_eq!(forms.possessive.as_deref(), Some("john"));
            assert!(forms.exceptions.is_empty());
            assert!(forms.rules.is_empty());
        }
        for word in ["children's", "children’s"] {
            let forms = candidates(word).unwrap();
            assert_eq!(forms.possessive.as_deref(), Some("children"));
            assert_eq!(forms.exceptions, ["child"]);
        }
        for word in ["dogs'", "dogs’", "DOGS'", "DOGS’"] {
            let forms = candidates(word).unwrap();
            assert_eq!(forms.possessive.as_deref(), Some("dogs"));
            assert_eq!(forms.rules, ["dog"]);
        }
        let forms = candidates("ThUnDeRs").unwrap();
        assert!(forms.possessive.is_none());
        assert_eq!(forms.rules, ["thunder"]);
    }

    #[test]
    fn gate_rejects_non_words_and_bounds_original_bytes() {
        for word in [
            "",
            "привет",
            "café",
            "cafe\u{301}",
            "two words",
            "two\twords",
            "two\nwords",
            " word",
            "word ",
            "dog1",
            "dog-cat",
            "don't",
            "john‘s",
            "john'",
            "'s",
            "’s",
            "s'",
            "s’",
            "S'",
            "john's's",
        ] {
            assert!(candidates(word).is_none(), "{word:?}");
        }
        assert!(candidates(&"a".repeat(64)).is_some());
        assert!(candidates(&"a".repeat(65)).is_none());
        assert!(candidates(&format!("{}'s", "a".repeat(62))).is_some());
        assert!(candidates(&format!("{}'s", "a".repeat(63))).is_none());
        assert!(candidates(&format!("{}’s", "a".repeat(60))).is_some());
        assert!(candidates(&format!("{}’s", "a".repeat(61))).is_none());
    }

    #[test]
    fn short_rule_outputs_are_dropped_and_rules_are_unique() {
        for word in ["s", "as", "es", "ies", "ied", "ier", "iest", "ed", "ing", "er", "est", "aing", "aar"] {
            let forms = candidates(word).unwrap();
            for (i, rule) in forms.rules.iter().enumerate() {
                assert!(rule.len() >= 2, "{word} -> {rule}");
                assert!(!forms.rules[..i].contains(rule), "duplicate {word} -> {rule}");
            }
        }
        assert!(candidates("as").unwrap().rules.is_empty());
        // The minimum is for rule guesses, not possessives or curated lemmas.
        assert_eq!(candidates("a's").unwrap().possessive.as_deref(), Some("a"));
        assert_eq!(exception_lemmas("as a\n", "as"), Some("a"));
        assert_eq!(candidates("wolves").unwrap().rules.iter().filter(|s| *s == "wolve").count(), 1);
    }

    #[test]
    fn embedded_exceptions_are_sorted_lowercase_well_formed_and_searchable() {
        assert!(EXCEPTIONS.ends_with('\n'));
        let mut previous = "";
        for line in EXCEPTIONS.lines() {
            let (form, lemmas) = line.split_once(' ').expect("form and lemmas");
            assert!(form > previous, "unsorted or duplicate form: {form}");
            // Only plain-letter forms can pass the gate; lemmas may keep punctuation.
            assert!(form.bytes().all(|b| b.is_ascii_lowercase()), "unreachable form: {form}");
            previous = form;
            let mut seen = Vec::new();
            for token in line.split(' ') {
                assert!(!token.is_empty());
                assert!(token.bytes().all(|b| b.is_ascii_lowercase() || matches!(b, b'\'' | b'-' | b'.')), "{token}");
                assert!(!token.contains('_'), "multi-word entry: {token}");
            }
            for lemma in lemmas.split(' ') {
                assert!(!seen.contains(&lemma), "duplicate lemma: {line}");
                seen.push(lemma);
            }
            // Every row is a search oracle, including the first and last rows.
            assert_eq!(exception_lemmas(EXCEPTIONS, form), Some(lemmas), "{form}");
        }
        for missing in ["", "a", "notawordnetexception", "zzzzzzzzzz"] {
            assert_eq!(exception_lemmas(EXCEPTIONS, missing), None, "{missing}");
        }
    }

    #[test]
    fn binary_search_handles_empty_single_line_and_missing_final_newline() {
        assert_eq!(exception_lemmas("", "a"), None);
        for text in ["a b", "a b\n"] {
            assert_eq!(exception_lemmas(text, "a"), Some("b"));
            assert_eq!(exception_lemmas(text, "b"), None);
            assert_eq!(exception_lemmas(text, ""), None);
        }
        for text in ["a b\nc d\ne f", "a b\nc d\ne f\n"] {
            for (word, expected) in [("a", "b"), ("c", "d"), ("e", "f")] {
                assert_eq!(exception_lemmas(text, word), Some(expected));
            }
            for word in ["", "b", "d", "z"] {
                assert_eq!(exception_lemmas(text, word), None);
            }
        }
    }
}
