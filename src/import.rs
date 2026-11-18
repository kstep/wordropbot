use std::str::FromStr;

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
const DASHES: [&str; 3] = ["-", "–", "—"];

/// Comparison form: NFC, lowercase, trimmed, inner whitespace collapsed. Accents are kept
/// because they are significant in Greek.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Normalized(String);

impl Normalized {
    pub fn new(text: &str) -> Normalized {
        let composed: String = text.nfc().collect();
        Normalized(
            composed
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A line that does not hold both a term and a translation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line is not a term and translation pair")]
pub struct BadLine;

impl FromStr for ParsedPair {
    type Err = BadLine;

    /// Splits one non-blank line into trimmed term and translation.
    fn from_str(line: &str) -> Result<ParsedPair, BadLine> {
        // Dashes are searched in the raw line so that `слово - ` is a bad line, not a spaced pair.
        let dash = DASH_SEPARATORS
            .iter()
            .filter_map(|sep| line.find(sep).map(|pos| (pos, sep.len())))
            .min_by_key(|&(pos, _)| pos);
        let (term, translation) = match dash {
            Some((pos, len)) => (&line[..pos], &line[pos + len..]),
            None => line.trim().split_once(char::is_whitespace).ok_or(BadLine)?,
        };
        let (term, translation) = (term.trim(), translation.trim());
        if [term, translation]
            .iter()
            .any(|side| side.is_empty() || DASHES.contains(side))
        {
            return Err(BadLine);
        }
        Ok(ParsedPair {
            term: term.into(),
            translation: translation.into(),
        })
    }
}

impl ParseReport {
    pub fn parse(text: &str, limits: &LimitsConfig) -> ParseReport {
        let mut report = ParseReport::default();
        for (idx, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match line.parse::<ParsedPair>() {
                Ok(pair)
                    if pair.term.chars().count() <= limits.max_term_chars
                        && pair.translation.chars().count() <= limits.max_translation_chars =>
                {
                    report.pairs.push(pair);
                }
                _ => report.bad_lines.push(idx + 1),
            }
        }
        report
    }
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
            ParseReport::parse("σπίτι дом", &limits()).pairs,
            [pair("σπίτι", "дом")]
        );
        assert_eq!(
            ParseReport::parse("run два слова", &limits()).pairs,
            [pair("run", "два слова")]
        );
    }
    #[test]
    fn dash_wins_over_space() {
        for line in ["το σπίτι - дом", "το σπίτι – дом", "το σπίτι — дом"]
        {
            assert_eq!(
                ParseReport::parse(line, &limits()).pairs,
                [pair("το σπίτι", "дом")],
                "{line}"
            );
        }
    }
    #[test]
    fn earliest_dash_separator_wins() {
        assert_eq!(
            ParseReport::parse("a — b - c", &limits()).pairs,
            [pair("a", "b - c")]
        );
    }
    #[test]
    fn reports_bad_lines_and_skips_blank() {
        let r = ParseReport::parse("a б\n\nодно\n  \nв г", &limits());
        assert_eq!(r.pairs, [pair("a", "б"), pair("в", "г")]);
        assert_eq!(r.bad_lines, [3]);
    }
    #[test]
    fn empty_side_of_dash_is_bad() {
        assert_eq!(ParseReport::parse("слово - ", &limits()).bad_lines, [1]);
        assert_eq!(ParseReport::parse(" - перевод", &limits()).bad_lines, [1]);
    }
    #[test]
    fn dash_only_side_is_bad() {
        for line in ["слово -", "слово –", "слово —", "- слово"] {
            assert_eq!(ParseReport::parse(line, &limits()).bad_lines, [1], "{line}");
        }
    }
    #[test]
    fn too_long_is_bad() {
        assert_eq!(
            ParseReport::parse("абвгдежзийк x", &limits()).bad_lines,
            [1]
        );
        assert_eq!(
            ParseReport::parse("x абвгдежзийк", &limits()).bad_lines,
            [1]
        );
    }
    #[test]
    fn pair_parses_through_from_str() {
        assert_eq!("σπίτι дом".parse(), Ok(pair("σπίτι", "дом")));
        assert_eq!("одно".parse::<ParsedPair>(), Err(BadLine));
    }
    #[test]
    fn normalizes() {
        assert_eq!(Normalized::new("  ΣΠΊΤΙ   Μου ").as_str(), "σπίτι μου");
        assert_eq!(Normalized::new("e\u{301}").as_str(), "é");
        assert_ne!(Normalized::new("σπίτι"), Normalized::new("σπιτι"));
    }
}
