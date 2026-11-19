use std::{fmt, ops::Add, str::FromStr, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use humantime::DurationError;
use serde::{Deserialize, Deserializer, de::Error as _};

use crate::ids::validated_sqlite_type;

const MAX_SECS: u64 = 365 * 86_400;

/// Whole seconds from one up to a year, so adding it to the current moment
/// never overflows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Period(i64);

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ParsePeriodError {
    #[error(transparent)]
    Invalid(#[from] DurationError),
    #[error("period must be from 1 second to 365 days")]
    OutOfRange,
}

impl Period {
    pub fn from_secs(seconds: u64) -> Option<Period> {
        (1..=MAX_SECS)
            .contains(&seconds)
            .then_some(Period(seconds as i64))
    }

    pub fn as_secs(self) -> i64 {
        self.0
    }
}

impl FromStr for Period {
    type Err = ParsePeriodError;

    /// Fractions of a second are dropped.
    fn from_str(input: &str) -> Result<Period, ParsePeriodError> {
        let duration = humantime::parse_duration(input.trim())?;
        Period::from_secs(duration.as_secs()).ok_or(ParsePeriodError::OutOfRange)
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

validated_sqlite_type!(Period, i64, i64, |seconds: i64| {
    u64::try_from(seconds)
        .ok()
        .and_then(Period::from_secs)
        .ok_or(ParsePeriodError::OutOfRange)
});

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
            (" 2h ", 7200),
            ("1h30m", 5400),
            ("1.5h", 5400),
            ("1w", 604800),
            ("90s", 90),
        ] {
            assert_eq!(text.parse::<Period>().unwrap().as_secs(), seconds, "{text}");
        }
    }

    #[test]
    fn rejects_invalid() {
        for text in ["", "h", "-1h", "soon"] {
            assert!(
                matches!(text.parse::<Period>(), Err(ParsePeriodError::Invalid(_))),
                "{text}"
            );
        }
        for text in ["0s", "500ms", "366d"] {
            assert_eq!(
                text.parse::<Period>(),
                Err(ParsePeriodError::OutOfRange),
                "{text}"
            );
        }
    }

    #[test]
    fn from_secs_checks_range() {
        assert_eq!(Period::from_secs(0), None);
        assert_eq!(Period::from_secs(MAX_SECS).unwrap().as_secs(), 31_536_000);
        assert_eq!(Period::from_secs(MAX_SECS + 1), None);
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
