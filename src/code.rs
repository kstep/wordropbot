#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    const ALPHABET: &str = "23456789ABCDEFGHJKMNPQRSTUVWXYZ";

    #[test]
    fn generated_code_has_format() {
        let mut rng = StdRng::seed_from_u64(1);
        let code = generate_code(&mut rng);
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        assert!(code.replace('-', "").chars().all(|c| ALPHABET.contains(c)));
    }

    #[test]
    fn normalizes_user_input() {
        assert_eq!(normalize_code("k7qm3xpa").as_deref(), Some("K7QM-3XPA"));
        assert_eq!(normalize_code(" K7QM-3XPA ").as_deref(), Some("K7QM-3XPA"));
    }

    #[test]
    fn rejects_bad_codes() {
        for s in ["", "K7QM", "K7QM-3XPAA", "O7QM-3XPA", "K1QM-3XPA", "K7QM-3XP!"] {
            assert_eq!(normalize_code(s), None, "{s}");
        }
    }
}
