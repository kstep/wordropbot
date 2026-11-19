use std::ops::Add;

use chrono::{DateTime, SubsecRound, TimeDelta, Utc, serde::ts_seconds};
use serde::Deserialize;
use sqlx::{
    Decode, Encode, Sqlite, Type,
    encode::IsNull,
    error::BoxDynError,
    sqlite::{SqliteArgumentsBuffer, SqliteTypeInfo, SqliteValueRef},
};

use crate::period::Period;

/// A moment in UTC with whole-second precision, stored as Unix seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(#[serde(with = "ts_seconds")] DateTime<Utc>);

impl Timestamp {
    /// `None` outside the range `chrono` represents.
    pub fn from_secs(seconds: i64) -> Option<Timestamp> {
        DateTime::from_timestamp(seconds, 0).map(Timestamp)
    }

    pub fn as_secs(self) -> i64 {
        self.0.timestamp()
    }

    /// Cut to whole seconds, so it compares equal to its stored form.
    pub fn now() -> Timestamp {
        Timestamp(Utc::now().trunc_subsecs(0))
    }

    /// Seconds from `earlier` to `self`, zero when `earlier` is in the future.
    pub fn secs_since(self, earlier: Timestamp) -> u64 {
        (self.0 - earlier.0).num_seconds().max(0).unsigned_abs()
    }
}

impl Add<Period> for Timestamp {
    type Output = Timestamp;

    /// Saturates at the latest moment `chrono` represents.
    fn add(self, period: Period) -> Timestamp {
        let moment = TimeDelta::try_seconds(period.as_secs())
            .and_then(|delta| self.0.checked_add_signed(delta))
            .unwrap_or(DateTime::<Utc>::MAX_UTC);
        Timestamp(moment)
    }
}

impl Type<Sqlite> for Timestamp {
    fn type_info() -> SqliteTypeInfo {
        <i64 as Type<Sqlite>>::type_info()
    }

    fn compatible(info: &SqliteTypeInfo) -> bool {
        <i64 as Type<Sqlite>>::compatible(info)
    }
}

impl Encode<'_, Sqlite> for Timestamp {
    fn encode_by_ref(&self, buffer: &mut SqliteArgumentsBuffer) -> Result<IsNull, BoxDynError> {
        <i64 as Encode<Sqlite>>::encode_by_ref(&self.as_secs(), buffer)
    }
}

impl<'r> Decode<'r, Sqlite> for Timestamp {
    fn decode(value: SqliteValueRef<'r>) -> Result<Timestamp, BoxDynError> {
        let seconds = <i64 as Decode<Sqlite>>::decode(value)?;
        Timestamp::from_secs(seconds)
            .ok_or_else(|| format!("timestamp {seconds} is out of range").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp(seconds: i64) -> Timestamp {
        Timestamp::from_secs(seconds).unwrap()
    }

    #[test]
    fn adds_period() {
        let period = Period::from_secs(60).unwrap();
        assert_eq!(timestamp(100) + period, timestamp(160));
    }

    #[test]
    fn addition_saturates() {
        let period = Period::from_secs(i64::MAX as u64).unwrap();
        assert_eq!(timestamp(100) + period, Timestamp(DateTime::<Utc>::MAX_UTC));
    }

    #[test]
    fn secs_since_clamps_future_to_zero() {
        let (early, late) = (timestamp(100), timestamp(160));
        assert_eq!(late.secs_since(early), 60);
        assert_eq!(early.secs_since(late), 0);
    }

    #[test]
    fn out_of_range_seconds_are_rejected() {
        assert_eq!(Timestamp::from_secs(i64::MAX), None);
    }
}
