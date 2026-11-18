#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_secs(secs)
    }

    #[test]
    fn blocks_after_limit_and_recovers() {
        let mut l = JoinLimiter::new(2);
        l.record_failure(UserId(1), at(0));
        assert!(l.allow(UserId(1), at(1)));
        l.record_failure(UserId(1), at(1));
        assert!(!l.allow(UserId(1), at(2)));
        assert!(l.allow(UserId(2), at(2)));
        assert!(l.allow(UserId(1), at(61)));
    }

    #[test]
    fn drops_expired_entries() {
        let mut l = JoinLimiter::new(2);
        l.record_failure(UserId(1), at(0));
        l.record_failure(UserId(2), at(100));
        assert_eq!(l.failures.len(), 1);
    }
}
