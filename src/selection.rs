use crate::config::SelectionConfig;

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
