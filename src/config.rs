#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"[telegram]
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
        assert_eq!(c.schedule.default_period, 86400);
        assert_eq!(c.schedule.min_period, 60);
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
