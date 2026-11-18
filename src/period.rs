use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer};

/// A positive whole number of seconds that fits in `i64`, the type SQLite stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Period(i64);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParsePeriodError {
    #[error("invalid period: {0}")]
    Invalid(String),
    #[error("period must be longer than zero")]
    Zero,
    #[error("period must be a whole number of seconds")]
    SubSecond,
    #[error("period is too large")]
    TooLarge,
}

impl Period {
    /// `None` for zero or more than `i64::MAX`.
    pub fn from_secs(secs: u64) -> Option<Period> {
        i64::try_from(secs).ok().filter(|&s| s > 0).map(Period)
    }

    pub fn as_secs(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for Period {
    type Error = ParsePeriodError;

    fn try_from(secs: i64) -> Result<Period, ParsePeriodError> {
        match secs {
            0 => Err(ParsePeriodError::Zero),
            s if s < 0 => Err(ParsePeriodError::Invalid(s.to_string())),
            s => Ok(Period(s)),
        }
    }
}

impl FromStr for Period {
    type Err = ParsePeriodError;

    fn from_str(s: &str) -> Result<Period, ParsePeriodError> {
        let text = s.trim().to_ascii_lowercase();
        let duration = humantime::parse_duration(&text)
            .map_err(|e| ParsePeriodError::Invalid(e.to_string()))?;
        if duration.is_zero() {
            return Err(ParsePeriodError::Zero);
        }
        if duration.subsec_nanos() != 0 {
            return Err(ParsePeriodError::SubSecond);
        }
        Period::from_secs(duration.as_secs()).ok_or(ParsePeriodError::TooLarge)
    }
}

impl fmt::Display for Period {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            humantime::format_duration(Duration::from_secs(self.0 as u64))
        )
    }
}

impl<'de> Deserialize<'de> for Period {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Period, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_forms() {
        for (text, secs) in [
            ("30m", 1800),
            ("2h", 7200),
            ("1d", 86400),
            (" 2H ", 7200),
            ("1h30m", 5400),
            ("1.5h", 5400),
            ("1w", 604800),
            ("90s", 90),
        ] {
            assert_eq!(text.parse::<Period>().unwrap().as_secs(), secs, "{text}");
        }
    }

    #[test]
    fn rejects_invalid() {
        assert_eq!("0m".parse::<Period>(), Err(ParsePeriodError::Zero));
        assert_eq!("0s".parse::<Period>(), Err(ParsePeriodError::Zero));
        assert_eq!("500ms".parse::<Period>(), Err(ParsePeriodError::SubSecond));
        assert_eq!("1.5s".parse::<Period>(), Err(ParsePeriodError::SubSecond));
        for s in ["", "h", "-1h", "soon", "99999999999999999999d"] {
            assert!(
                matches!(s.parse::<Period>(), Err(ParsePeriodError::Invalid(_))),
                "{s}"
            );
        }
    }

    #[test]
    fn from_secs_checks_range() {
        assert_eq!(Period::from_secs(0), None);
        assert_eq!(Period::from_secs(i64::MAX as u64 + 1), None);
        assert_eq!(Period::from_secs(60).unwrap().as_secs(), 60);
    }

    #[test]
    fn try_from_i64_checks_range() {
        assert_eq!(Period::try_from(0), Err(ParsePeriodError::Zero));
        assert!(matches!(
            Period::try_from(-5),
            Err(ParsePeriodError::Invalid(_))
        ));
        assert_eq!(Period::try_from(60).unwrap().as_secs(), 60);
    }

    #[test]
    fn display_roundtrips() {
        for secs in [1, 60, 90, 5400, 86400, 604800, 2_630_016] {
            let period = Period::from_secs(secs).unwrap();
            assert_eq!(
                period.to_string().parse::<Period>().unwrap(),
                period,
                "{period}"
            );
        }
    }
}
