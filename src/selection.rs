use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::config::SelectionConfig;

const SECS_PER_DAY: f64 = 86_400.0;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct AnswerRecord {
    pub answered_at: DateTime<Utc>,
    pub is_correct: bool,
}

impl SelectionConfig {
    /// Recency-weighted error rate, smoothed towards the configured prior.
    pub fn error_rate(&self, answers: &[AnswerRecord], now: DateTime<Utc>) -> f64 {
        let (mut total, mut errors) = (0.0, 0.0);
        for answer in answers {
            // Clock skew can date an answer in the future; clamping keeps the
            // weight at most 1.
            let age_days = (now - answer.answered_at).num_seconds().max(0) as f64 / SECS_PER_DAY;
            let weight = 0.5_f64.powf(age_days / self.half_life_days);
            total += weight;
            if !answer.is_correct {
                errors += weight;
            }
        }
        (errors + self.prior_error * self.prior_strength) / (total + self.prior_strength)
    }

    /// Selection weight of a word: `min_weight` floor plus the scaled error
    /// rate.
    pub fn word_weight(&self, answers: &[AnswerRecord], now: DateTime<Utc>) -> f64 {
        self.min_weight + (1.0 - self.min_weight) * self.error_rate(answers, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::timestamp;

    const DAY: i64 = 86_400;
    fn config() -> SelectionConfig {
        SelectionConfig::default()
    }
    fn correct(at: i64, count: usize) -> Vec<AnswerRecord> {
        vec![
            AnswerRecord {
                answered_at: timestamp(at),
                is_correct: true
            };
            count
        ]
    }
    #[test]
    fn word_weight_follows_answer_history() {
        let cases = [
            ("new word", Vec::new(), timestamp(0), 0.525, 1e-9),
            (
                "ten fresh correct",
                correct(1000, 10),
                timestamp(1000),
                0.0932,
                1e-3,
            ),
            (
                "ten correct 60 days ago",
                correct(0, 10),
                timestamp(60 * DAY),
                0.364,
                1e-3,
            ),
        ];
        for (name, answers, now, expected, tolerance) in cases {
            let weight = config().word_weight(&answers, now);
            assert!((weight - expected).abs() < tolerance, "{name}: {weight}");
        }
    }
    #[test]
    fn half_life_halves_answer_weight() {
        let config = SelectionConfig {
            prior_error: 0.0,
            ..config()
        };
        let wrong = [AnswerRecord {
            answered_at: timestamp(0),
            is_correct: false,
        }];
        // w = 0.5, p = 0.5 / (0.5 + 1)
        assert!((config.error_rate(&wrong, timestamp(14 * DAY)) - 1.0 / 3.0).abs() < 1e-9);
    }
    #[test]
    fn future_answer_has_zero_age() {
        assert_eq!(
            config().word_weight(&correct(500, 1), timestamp(100)),
            config().word_weight(&correct(100, 1), timestamp(100))
        );
    }
    #[test]
    fn weight_never_below_minimum() {
        let config = SelectionConfig {
            prior_error: 0.0,
            ..config()
        };
        assert!((config.word_weight(&correct(0, 1000), timestamp(0)) - 0.05).abs() < 1e-9);
    }
}
