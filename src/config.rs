use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};

use crate::period::parse_period;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Deserialize)]
pub struct TelegramConfig {
    pub token: String,
}

// Manual Debug so the token never reaches logs.
impl fmt::Debug for TelegramConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramConfig")
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct SelectionConfig {
    pub half_life_days: f64,
    pub prior_error: f64,
    pub prior_strength: f64,
    pub min_weight: f64,
}

#[derive(Debug, Deserialize)]
pub struct QuizConfig {
    pub options: usize,
}

#[derive(Debug, Deserialize)]
pub struct ScheduleConfig {
    #[serde(deserialize_with = "de_period")]
    pub default_period: u64,
    #[serde(deserialize_with = "de_period")]
    pub min_period: u64,
    pub tick_seconds: u64,
}

#[derive(Debug, Deserialize)]
pub struct LimitsConfig {
    pub max_term_chars: usize,
    pub max_translation_chars: usize,
    pub join_attempts_per_minute: u32,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub telegram: TelegramConfig,
    pub database: DatabaseConfig,
    pub selection: SelectionConfig,
    pub quiz: QuizConfig,
    pub schedule: ScheduleConfig,
    pub limits: LimitsConfig,
}

fn de_period<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let s = String::deserialize(d)?;
    parse_period(&s).ok_or_else(|| {
        serde::de::Error::custom(format!(
            "invalid duration {s:?}, expected e.g. \"30m\", \"2h\", \"1d\""
        ))
    })
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let c: Config = toml::from_str(text)?;
        c.validate()?;
        Ok(c)
    }

    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::parse(&text)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let s = &self.selection;
        let checks = [
            (
                !self.telegram.token.trim().is_empty(),
                "telegram.token must not be empty",
            ),
            (
                s.half_life_days > 0.0,
                "selection.half_life_days must be > 0",
            ),
            (
                (0.0..=1.0).contains(&s.prior_error),
                "selection.prior_error must be in 0..=1",
            ),
            (
                s.prior_strength > 0.0,
                "selection.prior_strength must be > 0",
            ),
            (
                s.min_weight > 0.0 && s.min_weight <= 1.0,
                "selection.min_weight must be in (0, 1]",
            ),
            (
                (2..=8).contains(&self.quiz.options),
                "quiz.options must be in 2..=8",
            ),
            (
                self.schedule.tick_seconds > 0,
                "schedule.tick_seconds must be > 0",
            ),
            (
                self.schedule.default_period >= self.schedule.min_period,
                "schedule.default_period must be >= schedule.min_period",
            ),
        ];
        match checks.iter().find(|(ok, _)| !ok) {
            Some((_, msg)) => Err(ConfigError::Invalid((*msg).to_owned())),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
pub fn test_config() -> Config {
    Config::parse(tests::VALID).expect("VALID config parses")
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) const VALID: &str = r#"[telegram]
token = "PUT_BOT_TOKEN_HERE"   # не пустой

[database]
path = "wordropbot.db"

[selection]
half_life_days = 14.0   # > 0
prior_error = 0.5       # 0..=1
prior_strength = 1.0    # > 0
min_weight = 0.05       # > 0 и <= 1

[quiz]
options = 3             # 2..=8, включая верный вариант

[schedule]
default_period = "1d"
min_period = "1m"
tick_seconds = 30       # > 0

[limits]
max_term_chars = 200
max_translation_chars = 200
join_attempts_per_minute = 5
"#;

    #[test]
    fn parses_valid() {
        let c = Config::parse(VALID).unwrap();
        assert_eq!(c.quiz.options, 3);
        assert_eq!(c.schedule.default_period.as_secs(), 86400);
        assert_eq!(c.schedule.min_period.as_secs(), 60);
    }

    #[test]
    fn ignores_unknown_keys() {
        assert!(Config::parse(&format!("{VALID}\n[extra]\nfoo = 1\n")).is_ok());
    }

    #[test]
    fn rejects_out_of_range() {
        for (from, to) in [
            ("half_life_days = 14.0", "half_life_days = 0.0"),
            ("prior_error = 0.5", "prior_error = 1.5"),
            ("prior_strength = 1.0", "prior_strength = 0.0"),
            ("min_weight = 0.05", "min_weight = 0.0"),
            ("min_weight = 0.05", "min_weight = 1.5"),
            ("options = 3", "options = 1"),
            ("options = 3", "options = 9"),
            ("tick_seconds = 30", "tick_seconds = 0"),
            ("default_period = \"1d\"", "default_period = \"soon\""),
            ("default_period = \"1d\"", "default_period = \"30s\""),
            ("token = \"PUT_BOT_TOKEN_HERE\"", "token = \"\""),
        ] {
            assert!(VALID.contains(from), "{from}");
            assert!(Config::parse(&VALID.replace(from, to)).is_err(), "{to}");
        }
    }

    #[test]
    fn rejects_missing_required_key() {
        assert!(Config::parse(&VALID.replace("[quiz]\noptions = 3", "[quiz]")).is_err());
    }

    #[test]
    fn debug_output_hides_token() {
        let c = Config::parse(VALID).unwrap();
        assert!(!format!("{c:?}").contains("PUT_BOT_TOKEN_HERE"));
    }
}
