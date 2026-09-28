//! Fuzzy filter over the script list (`/`), backed by nucleo-matcher.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

pub struct Fuzzy {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Fuzzy {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    /// Indices of the items matching `query`, best match first; ties keep the original
    /// order. An empty query matches everything in order. Smart case: lowercase queries
    /// ignore case.
    pub fn rank<'a>(&mut self, query: &str, items: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
        if query.trim().is_empty() {
            return items.into_iter().enumerate().map(|(i, _)| i).collect();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut scored: Vec<(u32, usize)> = items
            .into_iter()
            .enumerate()
            .filter_map(|(i, hay)| {
                let score = pattern.score(Utf32Str::new(hay, &mut self.buf), &mut self.matcher)?;
                Some((score, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ITEMS: &[&str] = &[
        "biolatency.bt Block I/O latency as a histogram.",
        "tcpconnect.bt Trace TCP connect()s.",
        "tcpaccept.bt Trace TCP accept()s",
        "opensnoop.bt Trace open() syscalls.",
    ];

    #[test]
    fn empty_query_keeps_everything_in_order() {
        assert_eq!(Fuzzy::new().rank("", ITEMS.iter().copied()), vec![0, 1, 2, 3]);
        assert_eq!(Fuzzy::new().rank("  ", ITEMS.iter().copied()), vec![0, 1, 2, 3]);
    }

    #[test]
    fn matches_name_and_description() {
        let mut f = Fuzzy::new();
        assert_eq!(f.rank("tcpcon", ITEMS.iter().copied()), vec![1]);
        // Fuzzy: "Trace open() syscalls" also contains t…c…p, but ranks below real hits.
        let tcp = f.rank("tcp", ITEMS.iter().copied());
        assert!(tcp[..2].contains(&1) && tcp[..2].contains(&2), "{tcp:?}");
        assert_eq!(f.rank("histogram", ITEMS.iter().copied()), vec![0]);
        assert_eq!(
            f.rank("HISTOGRAM", ITEMS.iter().copied()),
            Vec::<usize>::new(),
            "smart case"
        );
        assert_eq!(f.rank("zzz", ITEMS.iter().copied()), Vec::<usize>::new());
    }
}
