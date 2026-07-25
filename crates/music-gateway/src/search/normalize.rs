//! Text normalization shared by the index builder and the query path, so
//! both agree on tokenization. Mirrors the web `searchRanking.ts`
//! normalizer: lowercase, punctuation → separator, collapse whitespace.
//!
//! Diacritic folding is deliberately *not* done here — the fuzzy query's
//! edit-distance tolerance already absorbs single-accent differences
//! ("motorhead" ~ "Motörhead" is edit distance 1), so folding would add a
//! `unicode-normalization` dependency for a case the automaton handles.

/// Lowercase alphanumeric runs joined by single spaces; every other
/// character is a separator. `"Café (Deluxe Edition)"` → `"café deluxe
/// edition"`.
pub(crate) fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
        } else {
            // Defer the separator so trailing/duplicate separators never
            // produce empty tokens or a trailing space.
            pending_space = true;
        }
    }
    out
}

/// Normalized words. Empty input (or all-punctuation) yields an empty vec.
pub(crate) fn tokenize(s: &str) -> Vec<String> {
    let n = normalize(s);
    if n.is_empty() {
        return Vec::new();
    }
    n.split(' ').map(str::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_strips_punctuation() {
        assert_eq!(normalize("Café (Deluxe Edition)"), "café deluxe edition");
        assert_eq!(normalize("AC/DC"), "ac dc");
        assert_eq!(normalize("  Led   Zeppelin!! "), "led zeppelin");
    }

    #[test]
    fn no_empty_or_edge_tokens() {
        assert_eq!(tokenize("...The Knife..."), vec!["the", "knife"]);
        assert!(tokenize("   ").is_empty());
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn digits_are_kept() {
        assert_eq!(tokenize("Blink 182"), vec!["blink", "182"]);
    }
}
