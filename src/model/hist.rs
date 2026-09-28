//! Histogram math: bpftrace-style bucket labels and sub-cell bar widths.
//!
//! Labels follow bpftrace's text output (`src/output/text.cpp`, `hist_index_label`,
//! `lhist_index_label`): `[n]` for single-value buckets, `[a, b)` otherwise,
//! `(..., b)` for underflow and `[a, ...)` for overflow; log2 histograms print bounds that
//! are multiples of 1024 with K/M/G/T/P/E suffixes, linear ones only when the step is a
//! multiple of 1024.

use crate::bpftrace::json::Bucket;

/// Eighth blocks, 1/8 … 8/8 of a cell.
const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scale {
    Log2,
    /// Linear with this bucket width.
    Linear(i64),
}

/// Constant bucket width → `lhist`; anything else is a log2 `hist`.
fn scale(buckets: &[Bucket]) -> Scale {
    let mut widths = buckets
        .iter()
        .filter_map(|b| b.max?.checked_sub(b.min?)?.checked_add(1));
    let Some(first) = widths.next() else {
        return Scale::Log2;
    };
    if widths.all(|w| w == first) {
        Scale::Linear(first)
    } else {
        Scale::Log2
    }
}

fn bound(n: i64, scale: Scale) -> String {
    const SUFFIXES: [&str; 6] = ["K", "M", "G", "T", "P", "E"];
    let suffixed = match scale {
        Scale::Log2 => true,
        Scale::Linear(step) => step % 1024 == 0,
    };
    if !suffixed || n == 0 {
        return n.to_string();
    }
    let (mut value, mut decade) = (n, 0);
    let limit = match scale {
        Scale::Log2 => SUFFIXES.len(),
        Scale::Linear(_) => 2, // lhist only uses K and M
    };
    while decade < limit && value % 1024 == 0 {
        value /= 1024;
        decade += 1;
    }
    match decade {
        0 => value.to_string(),
        d => format!("{value}{}", SUFFIXES[d - 1]),
    }
}

/// Labels for `buckets`, in order.
pub fn labels(buckets: &[Bucket]) -> Vec<String> {
    let scale = scale(buckets);
    buckets
        .iter()
        .map(|b| match (b.min, b.max) {
            (Some(min), Some(max)) if min == max => format!("[{}]", bound(min, scale)),
            (Some(min), Some(max)) => {
                format!("[{}, {})", bound(min, scale), bound(max.saturating_add(1), scale))
            }
            (None, Some(max)) => format!("(..., {})", bound(max.saturating_add(1), scale)),
            (Some(min), None) => format!("[{}, ...)", bound(min, scale)),
            (None, None) => "?".to_string(),
        })
        .collect()
}

/// Like bpftrace: drop leading and trailing empty buckets, keep the gaps between.
pub fn trimmed(buckets: &[Bucket]) -> &[Bucket] {
    let first = buckets.iter().position(|b| b.count > 0);
    let last = buckets.iter().rposition(|b| b.count > 0);
    match (first, last) {
        (Some(f), Some(l)) => &buckets[f..=l],
        _ => &[],
    }
}

/// A bar of `count / max` × `width` cells, in eighths of a cell. Non-zero counts always
/// get at least one eighth so they are visible.
pub fn bar(count: u64, max: u64, width: usize) -> String {
    if max == 0 || width == 0 || count == 0 {
        return String::new();
    }
    let eighths = (u128::from(count) * width as u128 * 8 / u128::from(max)).max(1);
    let eighths = usize::try_from(eighths).unwrap_or(usize::MAX).min(width * 8);
    let mut s = "█".repeat(eighths / 8);
    if eighths % 8 > 0 {
        s.push(EIGHTHS[eighths % 8 - 1]);
    }
    s
}

/// Same, for signed/float values (tables): negative values get no bar.
pub fn bar_f64(value: f64, max: f64, width: usize) -> String {
    if !(value > 0.0 && max > 0.0) {
        return String::new();
    }
    let scaled = (value / max * 8_000.0).round().clamp(1.0, 8_000.0) as u64;
    bar(scaled, 8_000, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpftrace::json::{HistSeries, OutputMsg, parse_line};
    use pretty_assertions::assert_eq;

    fn b(min: Option<i64>, max: Option<i64>, count: u64) -> Bucket {
        Bucket { min, max, count }
    }

    fn fixture(name: &str) -> Vec<Bucket> {
        let text = std::fs::read_to_string(format!(
            "{}/tests/fixtures/json/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture");
        match parse_line(text.lines().next().expect("line")).remove(0) {
            OutputMsg::Hist {
                series: HistSeries::Single(buckets),
                ..
            } => buckets,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn log2_labels_like_bpftrace() {
        let buckets = vec![
            b(None, Some(-1), 1),
            b(Some(0), Some(0), 1),
            b(Some(1), Some(1), 0),
            b(Some(2), Some(3), 0),
            b(Some(512), Some(1023), 1),
            b(Some(1024), Some(2047), 2),
            b(Some(1 << 20), Some((2 << 20) - 1), 3),
            b(Some(1 << 30), None, 1),
        ];
        assert_eq!(
            labels(&buckets),
            vec![
                "(..., 0)",
                "[0]",
                "[1]",
                "[2, 4)",
                "[512, 1K)",
                "[1K, 2K)",
                "[1M, 2M)",
                "[1G, ...)"
            ]
        );
        assert_eq!(labels(&fixture("hist.ndjson"))[0], "[2, 4)");
        assert_eq!(labels(&fixture("hist.ndjson"))[9], "[1K, 2K)");
    }

    #[test]
    fn linear_labels() {
        let l = labels(&fixture("lhist.ndjson"));
        assert_eq!(l.first().map(String::as_str), Some("[0, 10)"));
        assert_eq!(l.last().map(String::as_str), Some("[100, ...)"));
        // Step multiple of 1024: K suffixes, like lhist_index_label.
        let kib = vec![
            b(Some(0), Some(1023), 1),
            b(Some(1024), Some(2047), 1),
            b(Some(3072), Some(4095), 1),
        ];
        assert_eq!(labels(&kib), vec!["[0, 1K)", "[1K, 2K)", "[3K, 4K)"]);
        // Otherwise plain numbers even above 1024.
        let plain = vec![b(Some(1000), Some(1999), 1), b(Some(2000), Some(2999), 1)];
        assert_eq!(labels(&plain), vec!["[1000, 2000)", "[2000, 3000)"]);
    }

    #[test]
    fn trimming_keeps_gaps() {
        let buckets = vec![
            b(Some(0), Some(0), 0),
            b(Some(1), Some(1), 2),
            b(Some(2), Some(3), 0),
            b(Some(4), Some(7), 1),
            b(Some(8), Some(15), 0),
        ];
        assert_eq!(trimmed(&buckets).len(), 3);
        assert!(trimmed(&[b(Some(0), Some(0), 0)]).is_empty());
    }

    #[test]
    fn bars_in_eighths() {
        assert_eq!(bar(10, 10, 4), "████");
        assert_eq!(bar(5, 10, 4), "██");
        assert_eq!(bar(1, 16, 2), "▏");
        assert_eq!(bar(3, 16, 2), "▍", "3/16 of 2 cells = 3/8 of a cell");
        assert_eq!(bar(1, 1_000_000, 10), "▏", "tiny but non-zero is visible");
        assert_eq!(bar(0, 10, 10), "");
        assert_eq!(bar(10, 0, 10), "");
        assert_eq!(bar(u64::MAX, u64::MAX, 3), "███");
        assert_eq!(bar_f64(5.0, 10.0, 4), "██");
        assert_eq!(bar_f64(-1.0, 10.0, 4), "");
    }
}
