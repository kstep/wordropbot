use std::str::FromStr;

use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

use crate::config::LimitsConfig;

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedPair {
    pub term: String,
    pub translation: String,
}

#[derive(Debug, Default)]
pub struct ParseReport {
    pub pairs: Vec<ParsedPair>,
    /// 1-based numbers of lines that could not be parsed; blank lines are
    /// counted but not reported.
    pub bad_lines: Vec<usize>,
}

const DASH_SEPARATORS: [&str; 3] = [" - ", " – ", " — "];
const DASHES: [&str; 3] = ["-", "–", "—"];

/// Comparison form: invisible format characters removed, NFC, lowercase,
/// trimmed, inner whitespace collapsed. Accents are kept because they are
/// significant in Greek.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Normalized(String);

impl Normalized {
    pub fn new(text: &str) -> Normalized {
        let lowercase = text
            .chars()
            .filter(|character| character.general_category() != GeneralCategory::Format)
            .nfc()
            .collect::<String>()
            .to_lowercase();
        // Lowercasing can leave sequences that NFC composes further.
        let composed: String = lowercase.nfc().collect();
        Normalized(composed.split_whitespace().collect::<Vec<_>>().join(" "))
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
        // Dashes are searched in the raw line so that `слово - ` is a bad line,
        // not a spaced pair.
        let dash = DASH_SEPARATORS
            .iter()
            .filter_map(|separator| {
                line.find(separator)
                    .map(|position| (position, separator.len()))
            })
            .min_by_key(|&(position, _)| position);
        let (term, translation) = match dash {
            Some((position, length)) => (&line[..position], &line[position + length..]),
            None => line.trim().split_once(char::is_whitespace).ok_or(BadLine)?,
        };
        let (term, translation) = (term.trim(), translation.trim());
        let is_blank = |side: &str| {
            let visible = Normalized::new(side);
            visible.as_str().is_empty() || DASHES.contains(&visible.as_str())
        };
        if is_blank(term) || is_blank(translation) {
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
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed: Result<ParsedPair, _> = line.parse();
            match parsed {
                Ok(pair)
                    if pair.term.chars().count() <= limits.max_term_chars
                        && pair.translation.chars().count() <= limits.max_translation_chars =>
                {
                    report.pairs.push(pair);
                }
                _ => report.bad_lines.push(index + 1),
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LimitsConfig;

    fn pair(term: &str, translation: &str) -> ParsedPair {
        ParsedPair {
            term: term.into(),
            translation: translation.into(),
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
        let report = ParseReport::parse("a б\n\nодно\n  \nв г", &limits());
        assert_eq!(report.pairs, [pair("a", "б"), pair("в", "г")]);
        assert_eq!(report.bad_lines, [3]);
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
    fn case_variants_of_composed_letters_match() {
        assert_eq!(
            Normalized::new("\u{3aa}\u{301}"),
            Normalized::new("\u{390}")
        );
        assert_eq!(
            Normalized::new("\u{3ab}\u{301}"),
            Normalized::new("\u{3b0}")
        );
    }
    #[test]
    fn invisible_characters_do_not_distinguish_text() {
        for text in ["дом\u{200b}", "до\u{ad}м", "\u{feff}дом", "д\u{2060}ом"] {
            assert_eq!(Normalized::new(text), Normalized::new("дом"), "{text:?}");
        }
    }
    #[test]
    fn side_of_only_invisible_characters_is_bad() {
        for line in ["x \u{200b}", "\u{200b} дом", "x - \u{ad}\u{200b}"] {
            assert_eq!(
                ParseReport::parse(line, &limits()).bad_lines,
                [1],
                "{line:?}"
            );
        }
    }
    #[test]
    fn normalizes() {
        assert_eq!(Normalized::new("  ΣΠΊΤΙ   Μου ").as_str(), "σπίτι μου");
        assert_eq!(Normalized::new("e\u{301}").as_str(), "é");
        assert_ne!(Normalized::new("σπίτι"), Normalized::new("σπιτι"));
    }
}
