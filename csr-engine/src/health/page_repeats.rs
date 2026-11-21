//! Repeated text on the probe's result pages: of the rows the known-item
//! probe's queries got back, how many hold the same normalized text as an
//! earlier row of the same page. This is the figure a reader of one page sees;
//! the storage figure (`duplicate_report`) counts identical chunk rows in the
//! whole table, most of which never meet on one page.

use std::collections::HashSet;

/// One row of a probe page: its conversation, and the key the same-text
/// collapse groups its text by (whitespace and case folded). `None` for a row
/// too short to collapse, which never counts as a repeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRow {
    pub conversation_id: String,
    pub same_text: Option<[u8; 32]>,
}

/// One probe page: its rows in order, and how many copies the same-text
/// collapse left out of the candidate pool the page was cut from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbePage {
    pub rows: Vec<ProbeRow>,
    pub same_text_collapsed: usize,
}

/// Rows over every probe page, and the rows whose text an earlier row of the
/// same page already holds. A text met again on another page is not a repeat.
/// The pages are read after the same-text collapse, which removes such rows,
/// so `repeats` stays near zero unless `CSR_NO_SAME_TEXT_COLLAPSE=1`;
/// `collapsed` counts the copies the collapse took out of the pools.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PageRepeats {
    pub pages: usize,
    pub rows: usize,
    pub repeats: usize,
    pub collapsed: usize,
}

impl PageRepeats {
    pub fn add_page(&mut self, page: &ProbePage) {
        let mut seen = HashSet::new();
        self.pages += 1;
        self.rows += page.rows.len();
        self.collapsed += page.same_text_collapsed;
        self.repeats += page
            .rows
            .iter()
            .filter_map(|row| row.same_text)
            .filter(|key| !seen.insert(*key))
            .count();
    }

    /// `repeats / rows`; `None` before any row was seen.
    pub fn rate(&self) -> Option<f64> {
        (self.rows > 0).then(|| self.repeats as f64 / self.rows as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(keys: &[Option<u8>], same_text_collapsed: usize) -> ProbePage {
        ProbePage {
            rows: keys
                .iter()
                .map(|key| ProbeRow {
                    conversation_id: "c".into(),
                    same_text: key.map(|k| [k; 32]),
                })
                .collect(),
            same_text_collapsed,
        }
    }

    #[test]
    fn a_row_repeats_only_an_earlier_row_of_its_own_page() {
        let mut tally = PageRepeats::default();
        // Rows 3 and 5 hold row 1's text; row 4 is too short to compare.
        tally.add_page(&page(&[Some(1), Some(2), Some(1), None, Some(1)], 4));
        assert_eq!(
            tally,
            PageRepeats {
                pages: 1,
                rows: 5,
                repeats: 2,
                collapsed: 4
            }
        );
        // The same text on a second page is that page's first copy, not a repeat.
        tally.add_page(&page(&[Some(1), None, None], 0));
        assert_eq!((tally.pages, tally.rows, tally.repeats), (2, 8, 2));
        assert_eq!(tally.collapsed, 4);
        assert_eq!(tally.rate(), Some(0.25));
    }

    #[test]
    fn short_rows_never_repeat_and_no_rows_give_no_rate() {
        let mut tally = PageRepeats::default();
        assert_eq!(tally.rate(), None);
        tally.add_page(&page(&[None, None], 0));
        tally.add_page(&page(&[], 0));
        assert_eq!((tally.pages, tally.rows, tally.repeats), (2, 2, 0));
        assert_eq!(tally.rate(), Some(0.0));
    }
}
