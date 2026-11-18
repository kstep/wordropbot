use std::collections::{HashMap, VecDeque};

use crate::{ids::UserId, time::Timestamp};

const WINDOW_SECS: u64 = 60;

/// Times of recent failed attempts, oldest first.
#[derive(Default)]
struct FailureWindow(VecDeque<Timestamp>);

impl FailureWindow {
    fn expire(&mut self, now: Timestamp) {
        while self
            .0
            .front()
            .is_some_and(|&time| now.secs_since(time) >= WINDOW_SECS)
        {
            self.0.pop_front();
        }
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn push(&mut self, now: Timestamp) {
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

    pub fn allow(&mut self, user: UserId, now: Timestamp) -> bool {
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

    pub fn record_failure(&mut self, user: UserId, now: Timestamp) {
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

    fn at(secs: i64) -> Timestamp {
        Timestamp::from_secs(secs)
    }

    #[test]
    fn blocks_after_limit_and_recovers() {
        let mut limiter = JoinLimiter::new(2);
        limiter.record_failure(UserId(1), at(0));
        assert!(limiter.allow(UserId(1), at(1)));
        limiter.record_failure(UserId(1), at(1));
        assert!(!limiter.allow(UserId(1), at(2)));
        assert!(limiter.allow(UserId(2), at(2)));
        assert!(limiter.allow(UserId(1), at(61)));
    }

    #[test]
    fn drops_expired_entries() {
        let mut limiter = JoinLimiter::new(2);
        limiter.record_failure(UserId(1), at(0));
        limiter.record_failure(UserId(2), at(100));
        assert_eq!(limiter.failures.len(), 1);
    }
}
