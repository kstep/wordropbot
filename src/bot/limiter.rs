use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, TimeDelta, Utc};

use crate::ids::UserId;

const WINDOW: TimeDelta = TimeDelta::minutes(1);

/// Times of recent failed attempts, oldest first.
#[derive(Default)]
struct FailureWindow(VecDeque<DateTime<Utc>>);

impl FailureWindow {
    fn expire(&mut self, now: DateTime<Utc>) {
        while self.0.front().is_some_and(|&time| now - time >= WINDOW) {
            self.0.pop_front();
        }
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn push(&mut self, now: DateTime<Utc>) {
        self.0.push_back(now);
    }
}

/// Per-user limit on failed join attempts inside a sliding 60-second window.
pub struct JoinLimiter {
    max_per_minute: u32,
    failures: HashMap<UserId, FailureWindow>,
}

impl JoinLimiter {
    pub fn new(max_per_minute: u32) -> Self {
        JoinLimiter {
            max_per_minute,
            failures: HashMap::new(),
        }
    }

    pub fn allow(&mut self, user: UserId, now: DateTime<Utc>) -> bool {
        let Some(window) = self.failures.get_mut(&user) else {
            return true;
        };
        window.expire(now);
        if window.is_empty() {
            self.failures.remove(&user);
            return true;
        }
        window.len() < self.max_per_minute as usize
    }

    pub fn record_failure(&mut self, user: UserId, now: DateTime<Utc>) {
        // Sweep every user so entries of idle users do not accumulate forever.
        self.failures.retain(|_, window| {
            window.expire(now);
            !window.is_empty()
        });
        self.failures.entry(user).or_default().push(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::timestamp;

    #[test]
    fn blocks_after_limit_and_recovers() {
        let mut limiter = JoinLimiter::new(2);
        limiter.record_failure(UserId(1), timestamp(0));
        assert!(limiter.allow(UserId(1), timestamp(1)));
        limiter.record_failure(UserId(1), timestamp(1));
        assert!(!limiter.allow(UserId(1), timestamp(2)));
        assert!(limiter.allow(UserId(2), timestamp(2)));
        assert!(limiter.allow(UserId(1), timestamp(61)));
    }

    #[test]
    fn drops_expired_entries() {
        let mut limiter = JoinLimiter::new(2);
        limiter.record_failure(UserId(1), timestamp(0));
        limiter.record_failure(UserId(2), timestamp(100));
        assert_eq!(limiter.failures.len(), 1);
    }
}
