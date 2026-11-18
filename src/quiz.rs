use std::collections::HashSet;

use rand::Rng;
use rand::seq::{IndexedRandom, SliceRandom};

use crate::import::Normalized;

pub struct QuizOptions {
    // Private so `correct_index` always points into `options`.
    options: Vec<String>,
    correct_index: usize,
}

impl QuizOptions {
    /// One correct option from `own` plus `count - 1` distractors from `pool`, shuffled.
    /// `None` if there are not enough distinct distractors.
    pub fn new<R: Rng + ?Sized>(
        own: &[String],
        pool: &[String],
        count: usize,
        rng: &mut R,
    ) -> Option<QuizOptions> {
        if count < 2 {
            return None;
        }
        let correct = own.choose(rng)?;
        // Excludes every translation of the word, not only the chosen one, so a synonym never
        // shows up as a wrong answer.
        let mut seen: HashSet<Normalized> = own.iter().map(|o| Normalized::new(o)).collect();
        let distractors: Vec<&String> = pool
            .iter()
            .filter(|p| seen.insert(Normalized::new(p)))
            .collect();
        if distractors.len() < count - 1 {
            return None;
        }
        let mut options: Vec<(&String, bool)> = distractors
            .choose_multiple(rng, count - 1)
            .map(|d| (*d, false))
            .collect();
        options.push((correct, true));
        options.shuffle(rng);
        let correct_index = options.iter().position(|&(_, is_correct)| is_correct)?;
        Some(QuizOptions {
            options: options.into_iter().map(|(o, _)| o.clone()).collect(),
            correct_index,
        })
    }

    pub fn options(&self) -> &[String] {
        &self.options
    }

    pub fn correct_index(&self) -> usize {
        self.correct_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn builds_requested_count_with_one_correct() {
        let mut rng = StdRng::seed_from_u64(1);
        let own = s(&["бежать", "управлять"]);
        let q = QuizOptions::new(&own, &s(&["стол", "груша", "окно"]), 3, &mut rng).unwrap();
        assert_eq!(q.options().len(), 3);
        assert!(own.contains(&q.options()[q.correct_index()]));
        let correct = q.options().iter().filter(|o| own.contains(o)).count();
        assert_eq!(correct, 1);
    }

    #[test]
    fn excludes_synonyms_by_normalized_form() {
        let mut rng = StdRng::seed_from_u64(1);
        let q = QuizOptions::new(
            &s(&["дом"]),
            &s(&["ДОМ", " дом ", "стол", "окно"]),
            3,
            &mut rng,
        )
        .unwrap();
        assert_eq!(
            q.options()
                .iter()
                .filter(|o| Normalized::new(o).as_str() == "дом")
                .count(),
            1
        );
    }

    #[test]
    fn duplicate_distractors_collapse() {
        let mut rng = StdRng::seed_from_u64(1);
        assert!(QuizOptions::new(&s(&["дом"]), &s(&["стол", "Стол"]), 3, &mut rng).is_none());
    }

    #[test]
    fn none_when_not_enough() {
        let mut rng = StdRng::seed_from_u64(1);
        assert!(QuizOptions::new(&s(&["дом"]), &s(&["стол"]), 3, &mut rng).is_none());
        assert!(QuizOptions::new(&[], &s(&["стол", "окно"]), 3, &mut rng).is_none());
    }
}
