use std::{fmt, ops::Add, str::FromStr, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Deserializer, de::Error as _};

use crate::ids::validated_sqlite_type;

/// A positive whole number of seconds, at most a year, so adding it to the
/// current moment never overflows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Period(i64);

const MAX_SECS: i64 = 365 * 86_400;

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
    /// `None` for zero or more than a year.
    pub fn from_secs(seconds: u64) -> Option<Period> {
        i64::try_from(seconds)
            .ok()
            .and_then(|seconds| Period::try_from(seconds).ok())
    }

    pub fn as_secs(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for Period {
    type Error = ParsePeriodError;

    fn try_from(seconds: i64) -> Result<Period, ParsePeriodError> {
        match seconds {
            0 => Err(ParsePeriodError::Zero),
            seconds if seconds < 0 => Err(ParsePeriodError::Invalid(seconds.to_string())),
            seconds if seconds > MAX_SECS => Err(ParsePeriodError::TooLarge),
            seconds => Ok(Period(seconds)),
        }
    }
}

impl FromStr for Period {
    type Err = ParsePeriodError;

    fn from_str(input: &str) -> Result<Period, ParsePeriodError> {
        // humantime reads `M` as months, and lowercasing would silently turn it
        // into minutes, so the ambiguous spelling is refused.
        let is_ambiguous = input
            .split(|character: char| !character.is_alphabetic())
            .any(|unit| unit == "M");
        if is_ambiguous {
            return Err(ParsePeriodError::Invalid(input.trim().to_owned()));
        }
        let text = input.trim().to_ascii_lowercase();
        let duration = humantime::parse_duration(&text)
            .map_err(|error| ParsePeriodError::Invalid(error.to_string()))?;
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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}",
            humantime::format_duration(Duration::from_secs(self.0 as u64))
        )
    }
}

validated_sqlite_type!(Period, i64, i64, Period::try_from);

impl Add<Period> for DateTime<Utc> {
    type Output = DateTime<Utc>;

    fn add(self, period: Period) -> DateTime<Utc> {
        self + TimeDelta::seconds(period.0)
    }
}

impl<'de> Deserialize<'de> for Period {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Period, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_forms() {
        for (text, seconds) in [
            ("30m", 1800),
            ("2h", 7200),
            ("1d", 86400),
            (" 2H ", 7200),
            ("1h30m", 5400),
            ("1.5h", 5400),
            ("1w", 604800),
            ("90s", 90),
            ("5MIN", 300),
        ] {
            assert_eq!(text.parse::<Period>().unwrap().as_secs(), seconds, "{text}");
        }
    }

    #[test]
    fn rejects_invalid() {
        assert_eq!("0m".parse::<Period>(), Err(ParsePeriodError::Zero));
        assert_eq!("0s".parse::<Period>(), Err(ParsePeriodError::Zero));
        assert_eq!("500ms".parse::<Period>(), Err(ParsePeriodError::SubSecond));
        assert_eq!("1.5s".parse::<Period>(), Err(ParsePeriodError::SubSecond));
        for text in [
            "",
            "h",
            "-1h",
            "soon",
            "99999999999999999999d",
            "1M",
            "1h 30M",
        ] {
            assert!(
                matches!(text.parse::<Period>(), Err(ParsePeriodError::Invalid(_))),
                "{text}"
            );
        }
    }

    #[test]
    fn from_secs_checks_range() {
        assert_eq!(Period::from_secs(0), None);
        assert_eq!(Period::from_secs(i64::MAX as u64 + 1), None);
        assert!(Period::from_secs(365 * 86_400).is_some());
        assert_eq!(Period::from_secs(365 * 86_400 + 1), None);
        assert_eq!("366d".parse::<Period>(), Err(ParsePeriodError::TooLarge));
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
        for seconds in [1, 60, 90, 5400, 86400, 604800, 2_630_016] {
            let period = Period::from_secs(seconds).unwrap();
            assert_eq!(
                period.to_string().parse::<Period>().unwrap(),
                period,
                "{period}"
            );
        }
    }
}
