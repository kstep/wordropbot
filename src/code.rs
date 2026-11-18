use std::{fmt, str::FromStr};

use rand::{CryptoRng, Rng};

use crate::ids::validated_sqlite_type;

const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
const HALF: usize = 4;

/// Collection code in canonical `XXXX-XXXX` form; the only way to build one is
/// `generate` or `FromStr`, so the format always holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Code(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseCodeError {
    #[error("code must have 8 characters")]
    InvalidLength,
    #[error("code contains a character outside the alphabet")]
    InvalidCharacter,
}

impl Code {
    /// Needs a CSPRNG because the code is the only credential for joining a
    /// collection.
    pub fn generate<R: CryptoRng + Rng + ?Sized>(random_generator: &mut R) -> Code {
        let mut code = String::with_capacity(2 * HALF + 1);
        for index in 0..2 * HALF {
            if index == HALF {
                code.push('-');
            }
            code.push(ALPHABET[random_generator.random_range(0..ALPHABET.len())] as char);
        }
        Code(code)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Code {
    type Err = ParseCodeError;

    /// Case, surrounding whitespace and the dash are ignored.
    fn from_str(input: &str) -> Result<Code, ParseCodeError> {
        let chars: Vec<char> = input
            .trim()
            .chars()
            .filter(|&character| character != '-')
            .map(|character| character.to_ascii_uppercase())
            .collect();
        if chars.len() != 2 * HALF {
            return Err(ParseCodeError::InvalidLength);
        }
        if !chars
            .iter()
            .all(|&character| character.is_ascii() && ALPHABET.contains(&(character as u8)))
        {
            return Err(ParseCodeError::InvalidCharacter);
        }
        let (head, tail) = chars.split_at(HALF);
        Ok(Code(format!(
            "{}-{}",
            head.iter().collect::<String>(),
            tail.iter().collect::<String>()
        )))
    }
}

validated_sqlite_type!(Code, str, &str, str::parse::<Code>);

impl fmt::Display for Code {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use rand::{SeedableRng, rngs::StdRng};

    use super::*;

    #[test]
    fn generated_code_has_canonical_format() {
        let mut random_generator = StdRng::seed_from_u64(1);
        let code = Code::generate(&mut random_generator);
        assert_eq!(code.as_str().len(), 9);
        assert_eq!(&code.as_str()[4..5], "-");
        assert!(
            code.as_str()
                .replace('-', "")
                .chars()
                .all(|character| ALPHABET.contains(&(character as u8)))
        );
    }

    #[test]
    fn parses_user_input() {
        assert_eq!("k7qm3xpa".parse::<Code>().unwrap().as_str(), "K7QM-3XPA");
        assert_eq!(" K7QM-3XPA ".parse::<Code>().unwrap().as_str(), "K7QM-3XPA");
    }

    #[test]
    fn reports_parse_errors() {
        for text in ["", "K7QM", "K7QM-3XPAA"] {
            assert_eq!(
                text.parse::<Code>(),
                Err(ParseCodeError::InvalidLength),
                "{text}"
            );
        }
        for text in ["O7QM-3XPA", "K1QM-3XPA", "K7QM-3XP!", "\u{132}7QM-3XPA"] {
            assert_eq!(
                text.parse::<Code>(),
                Err(ParseCodeError::InvalidCharacter),
                "{text}"
            );
        }
    }
}
