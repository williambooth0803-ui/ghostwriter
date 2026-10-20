//! Companion command labels (fork addition, see `docs/companion.md`), kept in `companion.tsv`
//! apart from the upstream catalogs and added to a language's catalog when it loads.

/// Language `code`'s companion entries, as catalog lines (`<TAB>source<TAB>translation`).
pub fn rows(code: &str) -> String {
    include_str!("companion.tsv")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix(code)?.strip_prefix('\t'))
        .map(|rest| format!("\t{rest}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn rows_are_per_language() {
        let de = super::rows("de");
        assert!(de.contains("\tAccept Suggestion\tVorschlag annehmen\n"));
        assert_eq!(de.lines().count(), 17);
        assert!(de.contains("\tStudy Notes and History\tLernnotizen und Verlauf\n"));
        // "sr" must not pick up "sr-latn" rows.
        assert_eq!(super::rows("sr").lines().count(), 17);
        assert!(super::rows("en").is_empty());
    }
}
