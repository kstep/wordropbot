use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::Deserialize;

use crate::period::Period;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    /// Only the message and line are kept: `toml::de::Error`'s `Display` quotes
    /// the offending source line, which could leak a malformed token into logs.
    #[error("invalid config{}: {message}", line.map_or(String::new(), |number| format!(" (line {number})")))]
    Parse {
        message: String,
        line: Option<usize>,
    },
    #[error("invalid config: {0}")]
    Invalid(String),
}

#[derive(Deserialize)]
pub struct TelegramConfig {
    pub token: String,
}

// Manual Debug so the token never reaches logs.
impl fmt::Debug for TelegramConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TelegramConfig")
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct SelectionConfig {
    pub half_life_days: f64,
    pub prior_error: f64,
    pub prior_strength: f64,
    pub min_weight: f64,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct QuizConfig {
    pub options: usize,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ScheduleConfig {
    pub default_period: Period,
    pub min_period: Period,
    pub tick_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    pub max_term_chars: usize,
    pub max_translation_chars: usize,
    pub join_attempts_per_minute: u32,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        DatabaseConfig {
            path: PathBuf::from("wordropbot.db"),
        }
    }
}

impl Default for SelectionConfig {
    fn default() -> Self {
        SelectionConfig {
            half_life_days: 14.0,
            prior_error: 0.5,
            prior_strength: 1.0,
            min_weight: 0.05,
        }
    }
}

impl Default for QuizConfig {
    fn default() -> Self {
        QuizConfig { options: 3 }
    }
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        ScheduleConfig {
            default_period: Period::from_secs(86_400).expect("1d is a valid period"),
            min_period: Period::from_secs(60).expect("1m is a valid period"),
            tick_seconds: 30,
        }
    }
}

impl Default for LimitsConfig {
    fn default() -> Self {
        LimitsConfig {
            max_term_chars: 200,
            max_translation_chars: 200,
            join_attempts_per_minute: 5,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Config {
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub selection: SelectionConfig,
    #[serde(default)]
    pub quiz: QuizConfig,
    #[serde(default)]
    pub schedule: ScheduleConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
}

impl FromStr for Config {
    type Err = ConfigError;

    fn from_str(text: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text).map_err(|error| ConfigError::Parse {
            message: error.message().to_owned(),
            line: error
                .span()
                .map(|span| text[..span.start].matches('\n').count() + 1),
        })?;
        config.validate()?;
        Ok(config)
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        text.parse()
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let selection = &self.selection;
        let checks = [
            (
                !self.telegram.token.trim().is_empty(),
                "telegram.token must not be empty",
            ),
            (
                selection.half_life_days.is_finite() && selection.half_life_days > 0.0,
                "selection.half_life_days must be > 0",
            ),
            (
                selection.prior_error.is_finite() && (0.0..=1.0).contains(&selection.prior_error),
                "selection.prior_error must be in 0..=1",
            ),
            (
                selection.prior_strength.is_finite() && selection.prior_strength > 0.0,
                "selection.prior_strength must be > 0",
            ),
            (
                selection.min_weight.is_finite()
                    && selection.min_weight > 0.0
                    && selection.min_weight <= 1.0,
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
            Some((_, message)) => Err(ConfigError::Invalid((*message).to_owned())),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
impl Config {
    pub fn for_tests() -> Config {
        tests::VALID.parse().expect("VALID config parses")
    }
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
    fn example_config_is_valid() {
        let config: Config = include_str!("../deploy/config.example.toml")
            .parse()
            .unwrap();
        assert_eq!(
            config.database.path,
            Path::new("/var/lib/wordropbot/wordropbot.db")
        );
    }

    #[test]
    fn parses_valid() {
        let config: Config = VALID.parse().unwrap();
        assert_eq!(config.quiz.options, 3);
        assert_eq!(config.schedule.default_period.as_secs(), 86400);
        assert_eq!(config.schedule.min_period.as_secs(), 60);
    }

    #[test]
    fn ignores_unknown_keys() {
        assert!(
            format!("{VALID}\n[extra]\nfoo = 1\n")
                .parse::<Config>()
                .is_ok()
        );
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
            ("prior_strength = 1.0", "prior_strength = inf"),
            ("half_life_days = 14.0", "half_life_days = inf"),
            ("half_life_days = 14.0", "half_life_days = nan"),
            ("prior_error = 0.5", "prior_error = nan"),
            ("tick_seconds = 30", "tick_seconds = 0"),
            ("default_period = \"1d\"", "default_period = \"soon\""),
            ("default_period = \"1d\"", "default_period = \"30s\""),
            ("token = \"PUT_BOT_TOKEN_HERE\"", "token = \"\""),
        ] {
            assert!(VALID.contains(from), "{from}");
            assert!(VALID.replace(from, to).parse::<Config>().is_err(), "{to}");
        }
    }

    const TOKEN_ONLY: &str = "[telegram]\ntoken = \"123:abc\"\n";

    #[test]
    fn token_only_config_uses_defaults() {
        let config: Config = TOKEN_ONLY.parse().unwrap();
        assert_eq!(config.database.path, Path::new("wordropbot.db"));
        assert_eq!(config.selection.half_life_days, 14.0);
        assert_eq!(config.selection.prior_error, 0.5);
        assert_eq!(config.selection.prior_strength, 1.0);
        assert_eq!(config.selection.min_weight, 0.05);
        assert_eq!(config.quiz.options, 3);
        assert_eq!(config.schedule.default_period.as_secs(), 86_400);
        assert_eq!(config.schedule.min_period.as_secs(), 60);
        assert_eq!(config.schedule.tick_seconds, 30);
        assert_eq!(config.limits.max_term_chars, 200);
        assert_eq!(config.limits.max_translation_chars, 200);
        assert_eq!(config.limits.join_attempts_per_minute, 5);
    }

    #[test]
    fn partial_section_keeps_other_defaults() {
        let config: Config = format!("{TOKEN_ONLY}[selection]\nmin_weight = 0.1\n")
            .parse()
            .unwrap();
        assert_eq!(config.selection.min_weight, 0.1);
        assert_eq!(config.selection.half_life_days, 14.0);
    }

    #[test]
    fn missing_token_is_rejected() {
        assert!("".parse::<Config>().is_err());
        assert!("[quiz]\noptions = 4\n".parse::<Config>().is_err());
    }

    #[test]
    fn defaults_are_still_validated() {
        let text = format!("{TOKEN_ONLY}[schedule]\nmin_period = \"2d\"\n");
        assert!(text.parse::<Config>().is_err());
    }

    #[test]
    fn parse_error_does_not_quote_source() {
        let result: Result<Config, _> = "[telegram]\ntoken = sEcReT\n".parse();
        let error = result.unwrap_err();
        assert!(!error.to_string().contains("sEcReT"), "{error}");
        assert!(
            matches!(error, ConfigError::Parse { line: Some(2), .. }),
            "{error}"
        );
    }

    #[test]
    fn debug_output_hides_token() {
        let config: Config = VALID.parse().unwrap();
        assert!(!format!("{config:?}").contains("PUT_BOT_TOKEN_HERE"));
    }
}
