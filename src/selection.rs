use rand::Rng;
use rand::distr::Distribution;
use rand::distr::weighted::WeightedIndex;

use crate::config::SelectionConfig;

const SECS_PER_DAY: f64 = 86_400.0;

/// One finished answer to a quiz about a word.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnswerRecord {
    pub answered_at: i64,
    pub is_correct: bool,
}

/// Recency-weighted error rate, smoothed towards the configured prior.
pub fn error_rate(answers: &[AnswerRecord], now: i64, cfg: &SelectionConfig) -> f64 {
    let (mut total, mut errors) = (0.0, 0.0);
    for a in answers {
        // Clock skew or future timestamps must not inflate the weight above 1.
        let age_days = (now - a.answered_at).max(0) as f64 / SECS_PER_DAY;
        let w = 0.5_f64.powf(age_days / cfg.half_life_days);
        total += w;
        if !a.is_correct {
            errors += w;
        }
    }
    (errors + cfg.prior_error * cfg.prior_strength) / (total + cfg.prior_strength)
}

/// Selection weight of a word: `min_weight` floor plus the scaled error rate.
pub fn word_weight(answers: &[AnswerRecord], now: i64, cfg: &SelectionConfig) -> f64 {
    cfg.min_weight + (1.0 - cfg.min_weight) * error_rate(answers, now, cfg)
}

/// Picks an index proportionally to `weights`; `None` if empty or all zero/invalid.
pub fn pick_weighted<R: Rng + ?Sized>(weights: &[f64], rng: &mut R) -> Option<usize> {
    WeightedIndex::new(weights).ok().map(|d| d.sample(rng))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    const DAY: i64 = 86_400;
    fn cfg() -> SelectionConfig {
        SelectionConfig { half_life_days: 14.0, prior_error: 0.5, prior_strength: 1.0, min_weight: 0.05 }
    }
    fn correct(at: i64, n: usize) -> Vec<AnswerRecord> {
        vec![AnswerRecord { answered_at: at, is_correct: true }; n]
    }
    #[test]
    fn new_word() {
        assert!((word_weight(&[], 0, &cfg()) - 0.525).abs() < 1e-9);
    }
    #[test]
    fn ten_fresh_correct() {
        assert!((word_weight(&correct(1000, 10), 1000, &cfg()) - 0.0932).abs() < 1e-3);
    }
    #[test]
    fn ten_correct_60_days_ago() {
        assert!((word_weight(&correct(0, 10), 60 * DAY, &cfg()) - 0.364).abs() < 1e-3);
    }
    #[test]
    fn half_life_halves_answer_weight() {
        let cfg = SelectionConfig { prior_error: 0.0, ..cfg() };
        let wrong = [AnswerRecord { answered_at: 0, is_correct: false }];
        // w = 0.5, p = 0.5 / (0.5 + 1)
        assert!((error_rate(&wrong, 14 * DAY, &cfg) - 1.0 / 3.0).abs() < 1e-9);
    }
    #[test]
    fn future_answer_has_zero_age() {
        assert_eq!(word_weight(&correct(500, 1), 100, &cfg()), word_weight(&correct(100, 1), 100, &cfg()));
    }
    #[test]
    fn weight_never_below_minimum() {
        let cfg = SelectionConfig { prior_error: 0.0, ..cfg() };
        assert!((word_weight(&correct(0, 1000), 0, &cfg) - 0.05).abs() < 1e-9);
    }
    #[test]
    fn pick_handles_edges() {
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(pick_weighted(&[], &mut rng), None);
        assert_eq!(pick_weighted(&[0.0, 1.0], &mut rng), Some(1));
    }
    #[test]
    fn pick_follows_weights() {
        let mut rng = StdRng::seed_from_u64(7);
        let hits = (0..10_000).filter(|_| pick_weighted(&[1.0, 3.0], &mut rng) == Some(1)).count();
        assert!((7_200..7_800).contains(&hits), "{hits}");
    }
}
