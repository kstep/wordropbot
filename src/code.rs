use std::fmt;
use std::str::FromStr;

use rand::{CryptoRng, Rng};
use sqlx::error::BoxDynError;
use sqlx::sqlite::{Sqlite, SqliteTypeInfo, SqliteValueRef};

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
    /// Needs a CSPRNG because the code is the only credential for joining a collection.
    pub fn generate<R: CryptoRng + Rng + ?Sized>(rng: &mut R) -> Code {
        let mut code = String::with_capacity(2 * HALF + 1);
        for i in 0..2 * HALF {
            if i == HALF {
                code.push('-');
            }
            code.push(ALPHABET[rng.random_range(0..ALPHABET.len())] as char);
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
            .filter(|&c| c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect();
        if chars.len() != 2 * HALF {
            return Err(ParseCodeError::InvalidLength);
        }
        if !chars
            .iter()
            .all(|&c| c.is_ascii() && ALPHABET.contains(&(c as u8)))
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

impl sqlx::Type<Sqlite> for Code {
    fn type_info() -> SqliteTypeInfo {
        <str as sqlx::Type<Sqlite>>::type_info()
    }

    fn compatible(info: &SqliteTypeInfo) -> bool {
        <str as sqlx::Type<Sqlite>>::compatible(info)
    }
}

/// A stored code in a non-canonical form is a decode error.
impl<'r> sqlx::Decode<'r, Sqlite> for Code {
    fn decode(value: SqliteValueRef<'r>) -> Result<Code, BoxDynError> {
        let raw = <&str as sqlx::Decode<Sqlite>>::decode(value)?;
        Ok(raw.parse()?)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
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
        assert!(
            code.as_str()
                .replace('-', "")
                .chars()
                .all(|c| ALPHABET.contains(c))
        );
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
            assert_eq!(
                s.parse::<Code>(),
                Err(ParseCodeError::InvalidCharacter),
                "{s}"
            );
        }
    }

    #[test]
    fn display_roundtrips() {
        let code: Code = "K7QM-3XPA".parse().unwrap();
        assert_eq!(code.to_string().parse::<Code>().unwrap(), code);
    }
}
