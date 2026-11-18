#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LimitsConfig;

    fn pair(t: &str, r: &str) -> ParsedPair {
        ParsedPair { term: t.into(), translation: r.into() }
    }
    fn limits() -> LimitsConfig {
        LimitsConfig { max_term_chars: 10, max_translation_chars: 10, join_attempts_per_minute: 5 }
    }

    #[test]
    fn splits_on_first_space() {
        assert_eq!(parse_pairs("σπίτι дом", &limits()).pairs, [pair("σπίτι", "дом")]);
        assert_eq!(parse_pairs("run два слова", &limits()).pairs, [pair("run", "два слова")]);
    }
    #[test]
    fn dash_wins_over_space() {
        for line in ["το σπίτι - дом", "το σπίτι – дом", "το σπίτι — дом"] {
            assert_eq!(parse_pairs(line, &limits()).pairs, [pair("το σπίτι", "дом")], "{line}");
        }
    }
    #[test]
    fn earliest_dash_separator_wins() {
        assert_eq!(parse_pairs("a — b - c", &limits()).pairs, [pair("a", "b - c")]);
    }
    #[test]
    fn reports_bad_lines_and_skips_blank() {
        let r = parse_pairs("a б\n\nодно\n  \nв г", &limits());
        assert_eq!(r.pairs, [pair("a", "б"), pair("в", "г")]);
        assert_eq!(r.bad_lines, [3]);
    }
    #[test]
    fn empty_side_of_dash_is_bad() {
        assert_eq!(parse_pairs("слово - ", &limits()).bad_lines, [1]);
        assert_eq!(parse_pairs(" - перевод", &limits()).bad_lines, [1]);
    }
    #[test]
    fn dash_only_side_is_bad() {
        for line in ["слово -", "слово –", "слово —", "- слово"] {
            assert_eq!(parse_pairs(line, &limits()).bad_lines, [1], "{line}");
        }
    }
    #[test]
    fn too_long_is_bad() {
        assert_eq!(parse_pairs("абвгдежзийк x", &limits()).bad_lines, [1]);
        assert_eq!(parse_pairs("x абвгдежзийк", &limits()).bad_lines, [1]);
    }
    #[test]
    fn normalizes() {
        assert_eq!(normalize("  ΣΠΊΤΙ   Μου "), "σπίτι μου");
        assert_eq!(normalize("e\u{301}"), "é");
        assert_ne!(normalize("σπίτι"), normalize("σπιτι"));
    }
}
