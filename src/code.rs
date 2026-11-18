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
    fn generated_code_has_canonical_format() {
        let mut rng = StdRng::seed_from_u64(1);
        let code = Code::generate(&mut rng);
        assert_eq!(code.as_str().len(), 9);
        assert_eq!(&code.as_str()[4..5], "-");
        assert!(code.as_str().replace('-', "").chars().all(|c| ALPHABET.contains(c)));
    }

    #[test]
    fn parses_user_input() {
        assert_eq!("k7qm3xpa".parse::<Code>().unwrap().as_str(), "K7QM-3XPA");
        assert_eq!(" K7QM-3XPA ".parse::<Code>().unwrap().as_str(), "K7QM-3XPA");
    }

    #[test]
    fn reports_parse_errors() {
        for s in ["", "K7QM", "K7QM-3XPAA"] {
            assert_eq!(s.parse::<Code>(), Err(ParseCodeError::InvalidLength), "{s}");
        }
        for s in ["O7QM-3XPA", "K1QM-3XPA", "K7QM-3XP!", "\u{132}7QM-3XPA"] {
            assert_eq!(s.parse::<Code>(), Err(ParseCodeError::InvalidCharacter), "{s}");
        }
    }

    #[test]
    fn display_roundtrips() {
        let code: Code = "K7QM-3XPA".parse().unwrap();
        assert_eq!(code.to_string().parse::<Code>().unwrap(), code);
    }
}
