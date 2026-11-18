use std::{
    ops::Add,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::period::Period;

/// Unix seconds, UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct Timestamp(i64);

impl Timestamp {
    pub const fn from_secs(secs: i64) -> Timestamp {
        Timestamp(secs)
    }

    pub const fn as_secs(self) -> i64 {
        self.0
    }

    pub fn now() -> Timestamp {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since_epoch| {
                i64::try_from(since_epoch.as_secs()).unwrap_or(i64::MAX)
            });
        Timestamp(secs)
    }

    /// Seconds from `earlier` to `self`, zero when `earlier` is in the future.
    pub fn secs_since(self, earlier: Timestamp) -> u64 {
        self.0.saturating_sub(earlier.0).max(0).unsigned_abs()
    }
}

impl Add<Period> for Timestamp {
    type Output = Timestamp;

    fn add(self, period: Period) -> Timestamp {
        Timestamp(self.0.saturating_add(period.as_secs()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_period() {
        let period = Period::from_secs(60).unwrap();
        assert_eq!(
            Timestamp::from_secs(100) + period,
            Timestamp::from_secs(160)
        );
    }

    #[test]
    fn addition_saturates() {
        let period = Period::from_secs(60).unwrap();
        assert_eq!(
            Timestamp::from_secs(i64::MAX) + period,
            Timestamp::from_secs(i64::MAX)
        );
    }

    #[test]
    fn secs_since_clamps_future_to_zero() {
        let (early, late) = (Timestamp::from_secs(100), Timestamp::from_secs(160));
        assert_eq!(late.secs_since(early), 60);
        assert_eq!(early.secs_since(late), 0);
    }
}
