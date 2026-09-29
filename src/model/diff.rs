//! Line diff between a script and its inline-edited draft (D-025, D-026). Pure.

/// Above this many line pairs the diff gives up on alignment and reports every line as
/// removed and added (scripts are far smaller; this only bounds the work).
const MAX_CELLS: usize = 4_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    Same(String),
    Added(String),
    Removed(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
}

impl Diff {
    /// `+3 −1`.
    pub fn summary(&self) -> String {
        format!("+{} −{}", self.added, self.removed)
    }

    /// Only the changes, with the line number they have in the old (`-`) or new (`+`)
    /// text: `+ 7 BEGIN { … }`.
    pub fn changes(&self) -> Vec<String> {
        let (mut old, mut new) = (0, 0);
        let mut out = Vec::new();
        for line in &self.lines {
            match line {
                DiffLine::Same(_) => {
                    old += 1;
                    new += 1;
                }
                DiffLine::Added(t) => {
                    new += 1;
                    out.push(format!("+{new:>4} {t}"));
                }
                DiffLine::Removed(t) => {
                    old += 1;
                    out.push(format!("-{old:>4} {t}"));
                }
            }
        }
        out
    }
}

/// Longest-common-subsequence line diff; removals come before additions at a change.
pub fn diff(old: &str, new: &str) -> Diff {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut lines = Vec::with_capacity(a.len().max(b.len()));
    if a.len().saturating_mul(b.len()) > MAX_CELLS {
        lines.extend(a.iter().map(|l| DiffLine::Removed(l.to_string())));
        lines.extend(b.iter().map(|l| DiffLine::Added(l.to_string())));
    } else {
        // lcs[i][j]: LCS length of a[i..] and b[j..].
        let w = b.len() + 1;
        let mut lcs = vec![0u32; (a.len() + 1) * w];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                lcs[i * w + j] = if a[i] == b[j] {
                    lcs[(i + 1) * w + j + 1] + 1
                } else {
                    lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            if i < a.len() && j < b.len() && a[i] == b[j] {
                lines.push(DiffLine::Same(a[i].to_string()));
                i += 1;
                j += 1;
            } else if i < a.len() && (j == b.len() || lcs[(i + 1) * w + j] >= lcs[i * w + j + 1]) {
                lines.push(DiffLine::Removed(a[i].to_string()));
                i += 1;
            } else {
                lines.push(DiffLine::Added(b[j].to_string()));
                j += 1;
            }
        }
    }
    let added = lines.iter().filter(|l| matches!(l, DiffLine::Added(_))).count();
    let removed = lines.iter().filter(|l| matches!(l, DiffLine::Removed(_))).count();
    Diff {
        lines,
        added,
        removed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DiffLine::*;
    use pretty_assertions::assert_eq;

    fn s(x: &str) -> String {
        x.to_string()
    }

    #[test]
    fn additions_removals_and_changes() {
        let d = diff("a\nb\nc\n", "a\nx\nb\nd\n");
        assert_eq!(
            d.lines,
            vec![
                Same(s("a")),
                Added(s("x")),
                Same(s("b")),
                Removed(s("c")),
                Added(s("d"))
            ]
        );
        assert_eq!((d.added, d.removed, d.summary()), (2, 1, s("+2 −1")));
        assert_eq!(d.changes(), vec![s("+   2 x"), s("-   3 c"), s("+   4 d")]);
    }

    #[test]
    fn edge_cases() {
        assert_eq!(diff("", "").lines, vec![]);
        assert_eq!(
            diff("a\n", "a").lines,
            vec![Same(s("a"))],
            "a final newline is no change"
        );
        assert_eq!(diff("", "a\nb").added, 2);
        assert_eq!(diff("a\nb", "").removed, 2);
        let same = diff("x\ny\n", "x\ny\n");
        assert_eq!((same.added, same.removed), (0, 0));
    }
}
