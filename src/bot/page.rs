use std::{fmt, num::ParseIntError, str::FromStr};

/// Zero-based index of a page in a list cut into fixed-size pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Page(u32);

impl Page {
    pub const FIRST: Page = Page(0);

    pub const fn new(index: u32) -> Page {
        Page(index)
    }

    /// The page that holds the item at `position`.
    pub fn of_item(position: usize, page_size: u32) -> Page {
        Page(u32::try_from(position / page_size as usize).unwrap_or(u32::MAX))
    }

    /// Last page of a list of `items`; an empty list still has the first page.
    pub fn last_for(items: usize, page_size: u32) -> Page {
        Page::of_item(items.saturating_sub(1), page_size)
    }

    /// Items on the pages before this one.
    pub fn offset(self, page_size: u32) -> u32 {
        self.0.saturating_mul(page_size)
    }

    /// One-based number shown to people.
    pub fn number(self) -> u32 {
        self.0.saturating_add(1)
    }

    pub fn previous(self) -> Option<Page> {
        self.0.checked_sub(1).map(Page)
    }

    pub fn next_within(self, last: Page) -> Option<Page> {
        (self < last).then(|| Page(self.0 + 1))
    }
}

/// The index, as carried in button data.
impl fmt::Display for Page {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl FromStr for Page {
    type Err = ParseIntError;

    fn from_str(text: &str) -> Result<Page, ParseIntError> {
        text.parse().map(Page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_page_of_list() {
        for (items, last) in [(0, 0), (1, 0), (20, 0), (21, 1), (40, 1), (41, 2)] {
            assert_eq!(Page::last_for(items, 20), Page::new(last), "{items}");
        }
    }

    #[test]
    fn item_falls_on_its_page() {
        assert_eq!(Page::of_item(0, 10), Page::FIRST);
        assert_eq!(Page::of_item(9, 10), Page::FIRST);
        assert_eq!(Page::of_item(10, 10), Page::new(1));
    }

    #[test]
    fn neighbours_exist_only_inside_list() {
        assert_eq!(Page::FIRST.previous(), None);
        assert_eq!(Page::new(1).previous(), Some(Page::FIRST));
        assert_eq!(Page::new(2).next_within(Page::new(2)), None);
        assert_eq!(Page::new(1).next_within(Page::new(2)), Some(Page::new(2)));
    }

    #[test]
    fn offset_skips_earlier_pages() {
        assert_eq!(Page::FIRST.offset(20), 0);
        assert_eq!(Page::new(2).offset(20), 40);
        assert_eq!(Page::new(u32::MAX).offset(20), u32::MAX);
    }

    #[test]
    fn parses_index() {
        assert_eq!("0".parse::<Page>(), Ok(Page::FIRST));
        assert!("-1".parse::<Page>().is_err());
        assert!("x".parse::<Page>().is_err());
    }
}
