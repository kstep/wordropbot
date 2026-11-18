use rand::{
    Rng,
    distr::{Distribution, weighted::WeightedIndex},
};

use crate::{config::SelectionConfig, time::Timestamp};

const SECS_PER_DAY: f64 = 86_400.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnswerRecord {
    pub answered_at: Timestamp,
    pub is_correct: bool,
}

impl SelectionConfig {
    /// Recency-weighted error rate, smoothed towards the configured prior.
    pub fn error_rate(&self, answers: &[AnswerRecord], now: Timestamp) -> f64 {
        let (mut total, mut errors) = (0.0, 0.0);
        for answer in answers {
            // `secs_since` clamps clock skew and future timestamps, so the
            // weight never exceeds 1.
            let age_days = now.secs_since(answer.answered_at) as f64 / SECS_PER_DAY;
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
    pub fn word_weight(&self, answers: &[AnswerRecord], now: Timestamp) -> f64 {
        self.min_weight + (1.0 - self.min_weight) * self.error_rate(answers, now)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Weights(Vec<f64>);

impl FromIterator<f64> for Weights {
    fn from_iter<I: IntoIterator<Item = f64>>(iter: I) -> Weights {
        Weights(iter.into_iter().collect())
    }
}

impl Weights {
    /// Picks an index proportionally to the weights; `None` if empty or all
    /// zero/invalid.
    pub fn pick<R: Rng + ?Sized>(&self, rng: &mut R) -> Option<usize> {
        WeightedIndex::new(&self.0)
            .ok()
            .map(|distribution| distribution.sample(rng))
    }
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng, rngs::StdRng};

    use super::*;
    use crate::testing::ts;

    const DAY: i64 = 86_400;
    fn config() -> SelectionConfig {
        SelectionConfig::default()
    }
    fn correct(at: i64, count: usize) -> Vec<AnswerRecord> {
        vec![
            AnswerRecord {
                answered_at: ts(at),
                is_correct: true
            };
            count
        ]
    }
    fn weights(values: &[f64]) -> Weights {
        values.iter().copied().collect()
    }
    #[test]
    fn word_weight_follows_answer_history() {
        let cases = [
            ("new word", Vec::new(), ts(0), 0.525, 1e-9),
            (
                "ten fresh correct",
                correct(1000, 10),
                ts(1000),
                0.0932,
                1e-3,
            ),
            (
                "ten correct 60 days ago",
                correct(0, 10),
                ts(60 * DAY),
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
            answered_at: ts(0),
            is_correct: false,
        }];
        // w = 0.5, p = 0.5 / (0.5 + 1)
        assert!((config.error_rate(&wrong, ts(14 * DAY)) - 1.0 / 3.0).abs() < 1e-9);
    }
    #[test]
    fn future_answer_has_zero_age() {
        assert_eq!(
            config().word_weight(&correct(500, 1), ts(100)),
            config().word_weight(&correct(100, 1), ts(100))
        );
    }
    #[test]
    fn weight_never_below_minimum() {
        let config = SelectionConfig {
            prior_error: 0.0,
            ..config()
        };
        assert!((config.word_weight(&correct(0, 1000), ts(0)) - 0.05).abs() < 1e-9);
    }
    #[test]
    fn pick_handles_edges() {
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(weights(&[]).pick(&mut rng), None);
        assert_eq!(weights(&[0.0, 1.0]).pick(&mut rng), Some(1));
    }
}
