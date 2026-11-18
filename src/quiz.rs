#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::normalize;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn builds_requested_count_with_one_correct() {
        let mut rng = StdRng::seed_from_u64(1);
        let own = s(&["бежать", "управлять"]);
        let q = build_options(&own, &s(&["стол", "груша", "окно"]), 3, &mut rng).unwrap();
        assert_eq!(q.options.len(), 3);
        assert!(own.contains(&q.options[q.correct_index]));
        let correct = q.options.iter().filter(|o| own.contains(o)).count();
        assert_eq!(correct, 1);
    }

    #[test]
    fn excludes_synonyms_by_normalized_form() {
        let mut rng = StdRng::seed_from_u64(1);
        let q = build_options(&s(&["дом"]), &s(&["ДОМ", " дом ", "стол", "окно"]), 3, &mut rng).unwrap();
        assert_eq!(q.options.iter().filter(|o| normalize(o) == "дом").count(), 1);
    }

    #[test]
    fn duplicate_distractors_collapse() {
        let mut rng = StdRng::seed_from_u64(1);
        assert!(build_options(&s(&["дом"]), &s(&["стол", "Стол"]), 3, &mut rng).is_none());
    }

    #[test]
    fn none_when_not_enough() {
        let mut rng = StdRng::seed_from_u64(1);
        assert!(build_options(&s(&["дом"]), &s(&["стол"]), 3, &mut rng).is_none());
        assert!(build_options(&[], &s(&["стол", "окно"]), 3, &mut rng).is_none());
    }
}
