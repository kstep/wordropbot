use unicode_normalization::UnicodeNormalization;

use crate::config::LimitsConfig;

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedPair {
    pub term: String,
    pub translation: String,
}

#[derive(Debug, Default)]
pub struct ParseReport {
    pub pairs: Vec<ParsedPair>,
    /// 1-based numbers of lines that could not be parsed; blank lines are counted but not reported.
    pub bad_lines: Vec<usize>,
}

const DASH_SEPARATORS: [&str; 3] = [" - ", " – ", " — "];

/// Comparison form: NFC, lowercase, trimmed, inner whitespace collapsed. Accents are kept
/// because they are significant in Greek.
pub fn normalize(s: &str) -> String {
    let composed: String = s.nfc().collect();
    composed
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_dash_only(s: &str) -> bool {
    matches!(s, "-" | "–" | "—")
}

/// Splits one non-blank line into trimmed term and translation, if it has both.
fn split_line(line: &str) -> Option<(&str, &str)> {
    // Dashes are searched in the raw line so that `слово - ` is a bad line, not a spaced pair.
    let dash = DASH_SEPARATORS
        .iter()
        .filter_map(|sep| line.find(sep).map(|pos| (pos, sep.len())))
        .min_by_key(|&(pos, _)| pos);
    let (term, translation) = match dash {
        Some((pos, len)) => (&line[..pos], &line[pos + len..]),
        None => line.trim().split_once(char::is_whitespace)?,
    };
    let (term, translation) = (term.trim(), translation.trim());
    if term.is_empty() || translation.is_empty() || is_dash_only(term) || is_dash_only(translation)
    {
        return None;
    }
    Some((term, translation))
}

pub fn parse_pairs(text: &str, limits: &LimitsConfig) -> ParseReport {
    let mut report = ParseReport::default();
    for (idx, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match split_line(line) {
            Some((term, translation))
                if term.chars().count() <= limits.max_term_chars
                    && translation.chars().count() <= limits.max_translation_chars =>
            {
                report.pairs.push(ParsedPair {
                    term: term.into(),
                    translation: translation.into(),
                });
            }
            _ => report.bad_lines.push(idx + 1),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LimitsConfig;

    fn pair(t: &str, r: &str) -> ParsedPair {
        ParsedPair {
            term: t.into(),
            translation: r.into(),
        }
    }
    fn limits() -> LimitsConfig {
        LimitsConfig {
            max_term_chars: 10,
            max_translation_chars: 10,
            join_attempts_per_minute: 5,
        }
    }

    #[test]
    fn splits_on_first_space() {
        assert_eq!(
            parse_pairs("σπίτι дом", &limits()).pairs,
            [pair("σπίτι", "дом")]
        );
        assert_eq!(
            parse_pairs("run два слова", &limits()).pairs,
            [pair("run", "два слова")]
        );
    }
    #[test]
    fn dash_wins_over_space() {
        for line in ["το σπίτι - дом", "το σπίτι – дом", "το σπίτι — дом"]
        {
            assert_eq!(
                parse_pairs(line, &limits()).pairs,
                [pair("το σπίτι", "дом")],
                "{line}"
            );
        }
    }
    #[test]
    fn earliest_dash_separator_wins() {
        assert_eq!(
            parse_pairs("a — b - c", &limits()).pairs,
            [pair("a", "b - c")]
        );
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
