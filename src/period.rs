use std::{fmt, str::FromStr, time::Duration};

use serde::{Deserialize, Deserializer, de::Error as _};
use sqlx::{
    error::BoxDynError,
    sqlite::{Sqlite, SqliteTypeInfo, SqliteValueRef},
};

/// A positive whole number of seconds that fits in `i64`, the type SQLite
/// stores.
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
        i64::try_from(secs)
            .ok()
            .filter(|&secs| secs > 0)
            .map(Period)
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
            secs if secs < 0 => Err(ParsePeriodError::Invalid(secs.to_string())),
            secs => Ok(Period(secs)),
        }
    }
}

impl FromStr for Period {
    type Err = ParsePeriodError;

    fn from_str(input: &str) -> Result<Period, ParsePeriodError> {
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

impl sqlx::Type<Sqlite> for Period {
    fn type_info() -> SqliteTypeInfo {
        <i64 as sqlx::Type<Sqlite>>::type_info()
    }

    fn compatible(info: &SqliteTypeInfo) -> bool {
        <i64 as sqlx::Type<Sqlite>>::compatible(info)
    }
}

/// A stored value outside the invariant is a decode error, not a panic.
impl<'r> sqlx::Decode<'r, Sqlite> for Period {
    fn decode(value: SqliteValueRef<'r>) -> Result<Period, BoxDynError> {
        let secs = <i64 as sqlx::Decode<Sqlite>>::decode(value)?;
        Ok(Period::try_from(secs)?)
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
        for text in ["", "h", "-1h", "soon", "99999999999999999999d"] {
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
