/// Parses `<digits><s|m|h|d>` into seconds; `None` for zero, overflow or bad syntax.
pub fn parse_period(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let unit = s.chars().last()?;
    let mult: u64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86400,
        _ => return None,
    };
    let digits = &s[..s.len() - 1];
    // `u64::from_str` would accept a leading '+', so check the digits explicitly.
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    if n == 0 {
        return None;
    }
    n.checked_mul(mult)
}

/// Formats seconds using the largest unit that divides them evenly.
pub fn format_period(secs: u64) -> String {
    for (mult, unit) in [(86400, "d"), (3600, "h"), (60, "m")] {
        if secs != 0 && secs.is_multiple_of(mult) {
            return format!("{}{unit}", secs / mult);
        }
    }
    format!("{secs}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_forms() {
        for (text, secs) in [
            ("30m", 1800),
            ("2h", 7200),
            ("1d", 86400),
            (" 2H ", 7200),
            ("1h30m", 5400),
            ("1.5h", 5400),
            ("1w", 604800),
            ("90s", 90),
        ] {
            assert_eq!(text.parse::<Period>().unwrap().as_secs(), secs, "{text}");
        }
    }

    #[test]
    fn rejects_invalid() {
        assert_eq!("0m".parse::<Period>(), Err(ParsePeriodError::Zero));
        assert_eq!("0s".parse::<Period>(), Err(ParsePeriodError::Zero));
        assert_eq!("500ms".parse::<Period>(), Err(ParsePeriodError::SubSecond));
        assert_eq!("1.5s".parse::<Period>(), Err(ParsePeriodError::SubSecond));
        for s in ["", "h", "-1h", "soon", "99999999999999999999d"] {
            assert!(
                matches!(s.parse::<Period>(), Err(ParsePeriodError::Invalid(_))),
                "{s}"
            );
        }
    }

    #[test]
    fn from_secs_checks_range() {
        assert_eq!(Period::from_secs(0), None);
        assert_eq!(Period::from_secs(i64::MAX as u64 + 1), None);
        assert_eq!(Period::from_secs(60).unwrap().as_secs(), 60);
    }

    #[test]
    fn display_roundtrips() {
        for secs in [1, 60, 90, 5400, 86400, 604800, 2_630_016] {
            let period = Period::from_secs(secs).unwrap();
            assert_eq!(period.to_string().parse::<Period>().unwrap(), period, "{period}");
        }
    }
}
