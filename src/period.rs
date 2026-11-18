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
        for s in ["", "h", "0m", "-1h", "+1h", "1w", "1.5h", "99999999999999999999d"] {
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
