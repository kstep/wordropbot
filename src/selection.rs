use rand::Rng;
use rand::distr::Distribution;
use rand::distr::weighted::WeightedIndex;

use crate::config::SelectionConfig;
use crate::time::Timestamp;

const SECS_PER_DAY: f64 = 86_400.0;

/// One finished answer to a quiz about a word.
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
            // `secs_since` clamps clock skew and future timestamps, so the weight never exceeds 1.
            let age_days = now.secs_since(answer.answered_at) as f64 / SECS_PER_DAY;
            let w = 0.5_f64.powf(age_days / self.half_life_days);
            total += w;
            if !answer.is_correct {
                errors += w;
            }
        }
        (errors + self.prior_error * self.prior_strength) / (total + self.prior_strength)
    }

    /// Selection weight of a word: `min_weight` floor plus the scaled error rate.
    pub fn word_weight(&self, answers: &[AnswerRecord], now: Timestamp) -> f64 {
        self.min_weight + (1.0 - self.min_weight) * self.error_rate(answers, now)
    }
}

/// Relative selection weights of a list of candidates.
#[derive(Clone, Debug, PartialEq)]
pub struct Weights(Vec<f64>);

impl FromIterator<f64> for Weights {
    fn from_iter<I: IntoIterator<Item = f64>>(iter: I) -> Weights {
        Weights(iter.into_iter().collect())
    }
}

impl Weights {
    /// Picks an index proportionally to the weights; `None` if empty or all zero/invalid.
    pub fn pick<R: Rng + ?Sized>(&self, rng: &mut R) -> Option<usize> {
        WeightedIndex::new(&self.0).ok().map(|d| d.sample(rng))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    const DAY: i64 = 86_400;
    fn cfg() -> SelectionConfig {
        SelectionConfig {
            half_life_days: 14.0,
            prior_error: 0.5,
            prior_strength: 1.0,
            min_weight: 0.05,
        }
    }
    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_secs(secs)
    }
    fn correct(at: i64, n: usize) -> Vec<AnswerRecord> {
        vec![
            AnswerRecord {
                answered_at: ts(at),
                is_correct: true
            };
            n
        ]
    }
    fn weights(w: &[f64]) -> Weights {
        w.iter().copied().collect()
    }
    #[test]
    fn new_word() {
        assert!((cfg().word_weight(&[], ts(0)) - 0.525).abs() < 1e-9);
    }
    #[test]
    fn ten_fresh_correct() {
        assert!((cfg().word_weight(&correct(1000, 10), ts(1000)) - 0.0932).abs() < 1e-3);
    }
    #[test]
    fn ten_correct_60_days_ago() {
        assert!((cfg().word_weight(&correct(0, 10), ts(60 * DAY)) - 0.364).abs() < 1e-3);
    }
    #[test]
    fn half_life_halves_answer_weight() {
        let cfg = SelectionConfig {
            prior_error: 0.0,
            ..cfg()
        };
        let wrong = [AnswerRecord {
            answered_at: ts(0),
            is_correct: false,
        }];
        // w = 0.5, p = 0.5 / (0.5 + 1)
        assert!((cfg.error_rate(&wrong, ts(14 * DAY)) - 1.0 / 3.0).abs() < 1e-9);
    }
    #[test]
    fn future_answer_has_zero_age() {
        assert_eq!(
            cfg().word_weight(&correct(500, 1), ts(100)),
            cfg().word_weight(&correct(100, 1), ts(100))
        );
    }
    #[test]
    fn weight_never_below_minimum() {
        let cfg = SelectionConfig {
            prior_error: 0.0,
            ..cfg()
        };
        assert!((cfg.word_weight(&correct(0, 1000), ts(0)) - 0.05).abs() < 1e-9);
    }
    #[test]
    fn pick_handles_edges() {
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(weights(&[]).pick(&mut rng), None);
        assert_eq!(weights(&[0.0, 1.0]).pick(&mut rng), Some(1));
    }
    #[test]
    fn pick_follows_weights() {
        let mut rng = StdRng::seed_from_u64(7);
        let hits = (0..10_000)
            .filter(|_| weights(&[1.0, 3.0]).pick(&mut rng) == Some(1))
            .count();
        assert!((7_200..7_800).contains(&hits), "{hits}");
    }
}
