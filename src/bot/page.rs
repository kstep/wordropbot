use std::{fmt, num::ParseIntError, str::FromStr};

/// 1-based page number of a list cut into fixed-size pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page(i64);

impl Page {
    pub const FIRST: Page = Page(1);

    /// Numbers below the first page mean the first page.
    pub fn new(number: i64) -> Page {
        Page(number.max(1))
    }

    /// Number of pages needed for `items`; an empty list still has one.
    pub fn count_for(items: i64, page_size: i64) -> i64 {
        (items.saturating_add(page_size - 1) / page_size).max(1)
    }

    pub fn clamped(self, total_pages: i64) -> Page {
        Page(self.0.clamp(1, total_pages.max(1)))
    }

    /// Items on the pages before this one.
    pub fn offset(self, page_size: i64) -> i64 {
        (self.0 - 1).saturating_mul(page_size)
    }

    pub fn previous(self) -> Option<Page> {
        (self.0 > 1).then(|| Page(self.0 - 1))
    }

    pub fn next_within(self, total_pages: i64) -> Option<Page> {
        (self.0 < total_pages).then(|| Page(self.0 + 1))
    }
}

impl fmt::Display for Page {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl FromStr for Page {
    type Err = ParseIntError;

    fn from_str(text: &str) -> Result<Page, ParseIntError> {
        text.parse().map(Page::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_pages_of_list() {
        for (items, expected) in [(0, 1), (1, 1), (20, 1), (21, 2), (40, 2), (41, 3)] {
            assert_eq!(Page::count_for(items, 20), expected, "{items}");
        }
    }

    #[test]
    fn numbers_stay_within_list() {
        assert_eq!(Page::new(-5), Page::FIRST);
        assert_eq!("0".parse::<Page>(), Ok(Page::FIRST));
        assert_eq!(Page::new(9).clamped(3), Page::new(3));
        assert_eq!(Page::new(2).clamped(3), Page::new(2));
        assert!("x".parse::<Page>().is_err());
    }

    #[test]
    fn neighbours_exist_only_inside_list() {
        assert_eq!(Page::FIRST.previous(), None);
        assert_eq!(Page::new(2).previous(), Some(Page::FIRST));
        assert_eq!(Page::new(3).next_within(3), None);
        assert_eq!(Page::new(2).next_within(3), Some(Page::new(3)));
    }

    #[test]
    fn offset_skips_earlier_pages() {
        assert_eq!(Page::FIRST.offset(20), 0);
        assert_eq!(Page::new(3).offset(20), 40);
    }
}
