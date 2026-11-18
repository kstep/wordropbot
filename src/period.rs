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
    fn parses_units() {
        assert_eq!(parse_period("30m"), Some(1800));
        assert_eq!(parse_period("2h"), Some(7200));
        assert_eq!(parse_period("1d"), Some(86400));
        assert_eq!(parse_period("45s"), Some(45));
        assert_eq!(parse_period(" 2H "), Some(7200));
    }

    #[test]
    fn rejects_invalid() {
        for s in [
            "",
            "h",
            "0m",
            "-1h",
            "+1h",
            "1w",
            "1.5h",
            "99999999999999999999d",
        ] {
            assert_eq!(parse_period(s), None, "{s}");
        }
    }

    #[test]
    fn formats_largest_whole_unit() {
        assert_eq!(format_period(7200), "2h");
        assert_eq!(format_period(90), "90s");
        assert_eq!(format_period(86400), "1d");
        assert_eq!(format_period(120), "2m");
    }
}
