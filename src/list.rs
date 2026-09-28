//! `--list`: print discovery + metadata as a plain-text table and exit. Debug aid (M1).

use crate::discovery::ScriptFile;
use crate::discovery::metadata::Metadata;

const HEADERS: [&str; 5] = ["ID", "PROBES", "PARAMS", "FLAGS", "DESCRIPTION"];

pub fn render(scripts: &[(ScriptFile, Metadata)]) -> String {
    let rows: Vec<[String; 5]> = scripts
        .iter()
        .map(|(file, meta)| {
            [
                file.id.clone(),
                probes(meta),
                params(meta),
                flags(meta),
                meta.description.clone().unwrap_or_default(),
            ]
        })
        .collect();

    let mut widths = HEADERS.map(|h| h.chars().count());
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }

    let mut out = String::new();
    let header = HEADERS.map(String::from);
    for row in std::iter::once(&header).chain(&rows) {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == row.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<w$}  ", w = widths[i]));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push_str(&format!("{} script(s)\n", rows.len()));
    out
}

/// Probe count; kernel/user probes (the ones validation must check) in parentheses.
fn probes(meta: &Metadata) -> String {
    let checked = meta.probes.iter().filter(|p| !p.always_available).count();
    format!("{} ({checked})", meta.probes.len())
}

fn params(meta: &Metadata) -> String {
    let positional = meta.params.positional.iter().map(|n| format!("${n}"));
    let named = meta.params.named.iter().map(|p| match (&p.default, p.is_bool) {
        (Some(d), false) => format!("--{}={d}", p.name),
        _ => format!("--{}", p.name),
    });
    positional.chain(named).collect::<Vec<_>>().join(" ")
}

fn flags(meta: &Metadata) -> String {
    let mut out = Vec::new();
    if meta.needs_unsafe() {
        out.push(format!("unsafe({})", meta.unsafe_calls.join(",")));
    }
    if meta.params.uses_argc {
        out.push("$#".to_string());
    }
    if !meta.usage.is_empty() {
        out.push("usage".to_string());
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::discovery::{metadata, walk};

    #[test]
    fn fixtures_table() {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scripts"));
        let scripts: Vec<_> = walk(root)
            .expect("walk")
            .scripts
            .into_iter()
            .map(|f| {
                let src = std::fs::read_to_string(&f.path).expect("read");
                (f, metadata::extract(&src))
            })
            .collect();
        insta::assert_snapshot!(render(&scripts));
    }
}
