use std::collections::{HashMap, VecDeque};

use crate::ids::UserId;
use crate::time::Timestamp;

const WINDOW_SECS: u64 = 60;

/// Per-user limit on failed join attempts inside a sliding 60-second window.
pub struct JoinLimiter {
    max_per_minute: u32,
    failures: HashMap<UserId, VecDeque<Timestamp>>,
}

impl JoinLimiter {
    pub fn new(max_per_minute: u32) -> Self {
        JoinLimiter {
            max_per_minute,
            failures: HashMap::new(),
        }
    }

    pub fn allow(&mut self, user: UserId, now: Timestamp) -> bool {
        let Some(times) = self.failures.get_mut(&user) else {
            return true;
        };
        expire(times, now);
        if times.is_empty() {
            self.failures.remove(&user);
            return true;
        }
        times.len() < self.max_per_minute as usize
    }

    pub fn record_failure(&mut self, user: UserId, now: Timestamp) {
        // Sweep every user so entries of idle users do not accumulate forever.
        self.failures.retain(|_, times| {
            expire(times, now);
            !times.is_empty()
        });
        self.failures.entry(user).or_default().push_back(now);
    }
}

fn expire(times: &mut VecDeque<Timestamp>, now: Timestamp) {
    while times
        .front()
        .is_some_and(|&t| now.secs_since(t) >= WINDOW_SECS)
    {
        times.pop_front();
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
