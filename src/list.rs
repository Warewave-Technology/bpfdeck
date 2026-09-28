//! `--list`: discovery + metadata (+ validation, when bpftrace is available) as a plain-text
//! table. Debug aid, see `headless.rs`.

use crate::bpftrace::validate::{Validation, Verdict};
use crate::discovery::ScriptFile;
use crate::discovery::metadata::Metadata;

const HEADERS: [&str; 6] = ["ID", "STATUS", "PROBES", "PARAMS", "FLAGS", "DESCRIPTION"];
const REASON_WIDTH: usize = 40;

pub struct Row {
    pub file: ScriptFile,
    pub meta: Metadata,
    /// `None` when validation did not run (no bpftrace).
    pub validation: Option<Validation>,
}

pub fn render(scripts: &[Row]) -> String {
    let rows: Vec<[String; 6]> = scripts
        .iter()
        .map(|row| {
            [
                row.file.id.clone(),
                status(row.validation.as_ref()),
                probes(&row.meta),
                params(&row.meta),
                flags(&row.meta),
                row.meta.description.clone().unwrap_or_default(),
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

/// Spec §5.1 glyphs, plus a short reason.
fn status(validation: Option<&Validation>) -> String {
    let Some(v) = validation else {
        return "-".to_string();
    };
    match &v.verdict {
        Verdict::Ok => "●".to_string(),
        Verdict::Partial { found, total } => format!("◐ {found}/{total} probes"),
        Verdict::NeedsUnsafe => "! needs --unsafe".to_string(),
        Verdict::Failed { reason } => {
            let mut short: String = reason.chars().take(REASON_WIDTH).collect();
            if reason.chars().count() > REASON_WIDTH {
                short.push('…');
            }
            format!("✗ {short}")
        }
    }
}

/// For every script that is not plainly OK: notes, per-probe results and raw output.
pub fn render_details(rows: &[Row]) -> String {
    let mut out = String::new();
    for row in rows {
        let Some(v) = &row.validation else { continue };
        if v.verdict == Verdict::Ok && v.notes.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "\n{} ({:?}): {:?}\n",
            row.file.id, v.strategy, v.verdict
        ));
        for note in &v.notes {
            out.push_str(&format!("  note: {note}\n"));
        }
        for p in &v.probes {
            out.push_str(&format!("  {} {}\n", if p.found { "✓" } else { "✗" }, p.probe));
        }
        for line in v.output.lines() {
            out.push_str(&format!("  | {line}\n"));
        }
    }
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
        let rows: Vec<_> = walk(root)
            .expect("walk")
            .scripts
            .into_iter()
            .map(|file| {
                let src = std::fs::read_to_string(&file.path).expect("read");
                let meta = metadata::extract(&src);
                Row {
                    file,
                    meta,
                    validation: None,
                }
            })
            .collect();
        insta::assert_snapshot!(render(&rows));
        assert_eq!(render_details(&rows), "");
    }

    #[test]
    fn status_glyphs_and_details() {
        use crate::bpftrace::validate::{ProbeCheck, Strategy};
        let v = |verdict| Validation {
            verdict,
            strategy: Strategy::ProbeList,
            output: String::new(),
            notes: Vec::new(),
            probes: Vec::new(),
        };
        assert_eq!(status(None), "-");
        assert_eq!(status(Some(&v(Verdict::Ok))), "●");
        assert_eq!(
            status(Some(&v(Verdict::Partial { found: 1, total: 3 }))),
            "◐ 1/3 probes"
        );
        assert_eq!(status(Some(&v(Verdict::NeedsUnsafe))), "! needs --unsafe");
        let long = Verdict::Failed {
            reason: "x".repeat(50),
        };
        assert_eq!(status(Some(&v(long))), format!("✗ {}…", "x".repeat(REASON_WIDTH)));

        let mut failed = v(Verdict::Failed {
            reason: "no probe found".into(),
        });
        failed.notes = vec!["heuristic".into()];
        failed.probes = vec![ProbeCheck {
            probe: "kprobe:x".into(),
            found: false,
        }];
        failed.output = "ERROR: a\nERROR: b".into();
        let row = Row {
            file: ScriptFile {
                id: "x.bt".into(),
                path: "/x.bt".into(),
                size: 1,
            },
            meta: Metadata::default(),
            validation: Some(failed),
        };
        insta::assert_snapshot!(render_details(&[row]));
    }
}
