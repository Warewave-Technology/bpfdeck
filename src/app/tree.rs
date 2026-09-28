//! Rows of the script list: flat (one per script) or a tree with collapsible directories.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListRow {
    /// Index into `App::entries`; `depth` is the indentation level in tree view.
    Script { entry: usize, depth: usize },
    Dir {
        /// Relative path without trailing slash (`tools/old`).
        path: String,
        depth: usize,
        expanded: bool,
        /// Scripts anywhere below this directory.
        scripts: usize,
    },
}

impl ListRow {
    /// Stable identity, to keep the selection across rebuilds.
    pub fn key(&self, ids: &[&str]) -> String {
        match self {
            Self::Script { entry, .. } => format!("s:{}", ids.get(*entry).copied().unwrap_or_default()),
            Self::Dir { path, .. } => format!("d:{path}"),
        }
    }
}

/// Directory prefixes of `id`: `a/b/c.bt` → `["a", "a/b"]`.
fn dirs_of(id: &str) -> Vec<&str> {
    id.match_indices('/').map(|(i, _)| &id[..i]).collect()
}

/// Flat rows, in the given order.
pub fn flat_rows(entries: &[usize]) -> Vec<ListRow> {
    entries
        .iter()
        .map(|&entry| ListRow::Script { entry, depth: 0 })
        .collect()
}

/// Tree rows for `scripts` (`(entry index, id)` sorted by id). A directory row precedes
/// its contents; contents of collapsed directories are left out.
pub fn tree_rows(scripts: &[(usize, &str)], collapsed: &HashSet<String>) -> Vec<ListRow> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (_, id) in scripts {
        for dir in dirs_of(id) {
            *counts.entry(dir).or_default() += 1;
        }
    }
    let mut rows = Vec::new();
    let mut open: Vec<&str> = Vec::new(); // directories on the path of the previous script
    for &(entry, id) in scripts {
        let dirs = dirs_of(id);
        let common = open.iter().zip(&dirs).take_while(|(a, b)| a == b).count();
        open.truncate(common);
        for &dir in &dirs[common..] {
            if !hidden(dir, collapsed) {
                rows.push(ListRow::Dir {
                    path: dir.to_string(),
                    depth: open.len(),
                    expanded: !collapsed.contains(dir),
                    scripts: counts.get(dir).copied().unwrap_or(0),
                });
            }
            open.push(dir);
        }
        if !dirs.iter().any(|d| collapsed.contains(*d)) {
            rows.push(ListRow::Script {
                entry,
                depth: dirs.len(),
            });
        }
    }
    rows
}

/// A directory row is hidden when one of its ancestors is collapsed.
fn hidden(dir: &str, collapsed: &HashSet<String>) -> bool {
    dirs_of(dir).iter().any(|d| collapsed.contains(*d))
}

/// The directory containing `row` (for "go to parent").
pub fn parent(row: &ListRow, ids: &[&str]) -> Option<String> {
    let path = match row {
        ListRow::Script { entry, .. } => ids.get(*entry)?.to_string(),
        ListRow::Dir { path, .. } => path.clone(),
    };
    path.rsplit_once('/').map(|(dir, _)| dir.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const IDS: &[&str] = &[
        "readme.bt",
        "tests/runtime/a.bt",
        "tests/runtime/b.bt",
        "tests/self/c.bt",
        "tools/biolatency.bt",
        "tools/old/biosnoop.bt",
        "tools/tcpconnect.bt",
    ];

    fn scripts() -> Vec<(usize, &'static str)> {
        IDS.iter().copied().enumerate().collect()
    }

    /// `  ▾ tools (3)` / `    tools/biolatency.bt` style, for readable assertions.
    fn render(rows: &[ListRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                ListRow::Dir {
                    path,
                    depth,
                    expanded,
                    scripts,
                } => {
                    format!(
                        "{}{} {path} ({scripts})",
                        "  ".repeat(*depth),
                        if *expanded { "▾" } else { "▸" }
                    )
                }
                ListRow::Script { entry, depth } => format!("{}{}", "  ".repeat(*depth), IDS[*entry]),
            })
            .collect()
    }

    #[test]
    fn expanded_tree() {
        assert_eq!(
            render(&tree_rows(&scripts(), &HashSet::new())),
            vec![
                "readme.bt",
                "▾ tests (3)",
                "  ▾ tests/runtime (2)",
                "    tests/runtime/a.bt",
                "    tests/runtime/b.bt",
                "  ▾ tests/self (1)",
                "    tests/self/c.bt",
                "▾ tools (3)",
                "  tools/biolatency.bt",
                "  ▾ tools/old (1)",
                "    tools/old/biosnoop.bt",
                "  tools/tcpconnect.bt",
            ]
        );
    }

    #[test]
    fn collapsed_directories_hide_their_contents() {
        let collapsed: HashSet<String> = ["tests".to_string(), "tools/old".to_string()].into();
        assert_eq!(
            render(&tree_rows(&scripts(), &collapsed)),
            vec![
                "readme.bt",
                "▸ tests (3)",
                "▾ tools (3)",
                "  tools/biolatency.bt",
                "  ▸ tools/old (1)",
                "  tools/tcpconnect.bt",
            ]
        );
    }

    #[test]
    fn keys_parents_and_flat() {
        let rows = tree_rows(&scripts(), &HashSet::new());
        assert_eq!(rows[1].key(IDS), "d:tests");
        assert_eq!(rows[3].key(IDS), "s:tests/runtime/a.bt");
        assert_eq!(parent(&rows[3], IDS).as_deref(), Some("tests/runtime"));
        assert_eq!(parent(&rows[2], IDS).as_deref(), Some("tests"));
        assert_eq!(parent(&rows[1], IDS), None);
        assert_eq!(parent(&rows[0], IDS), None);
        assert_eq!(
            flat_rows(&[2, 0]),
            vec![
                ListRow::Script { entry: 2, depth: 0 },
                ListRow::Script { entry: 0, depth: 0 }
            ]
        );
        assert!(tree_rows(&[], &HashSet::new()).is_empty());
    }
}
