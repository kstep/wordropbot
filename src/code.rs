use rand::{CryptoRng, Rng};

const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
const HALF: usize = 4;

/// Random collection code like `K7QM-3XPA`. Needs a CSPRNG because the code is the only
/// credential for joining a collection.
pub fn generate_code<R: CryptoRng + Rng + ?Sized>(rng: &mut R) -> String {
    let mut code = String::with_capacity(2 * HALF + 1);
    for i in 0..2 * HALF {
        if i == HALF {
            code.push('-');
        }
        code.push(ALPHABET[rng.random_range(0..ALPHABET.len())] as char);
    }
    code
}

/// Canonical `XXXX-XXXX` form of user input; case and the dash are ignored.
pub fn normalize_code(input: &str) -> Option<String> {
    let chars: Vec<char> = input
        .trim()
        .chars()
        .filter(|&c| c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if chars.len() != 2 * HALF
        || !chars
            .iter()
            .all(|&c| c.is_ascii() && ALPHABET.contains(&(c as u8)))
    {
        return None;
    }
    let (a, b) = chars.split_at(HALF);
    Some(format!(
        "{}-{}",
        a.iter().collect::<String>(),
        b.iter().collect::<String>()
    ))
}

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
        for s in [
            "",
            "K7QM",
            "K7QM-3XPAA",
            "O7QM-3XPA",
            "K1QM-3XPA",
            "K7QM-3XP!",
        ] {
            assert_eq!(normalize_code(s), None, "{s}");
        }
    }
}
