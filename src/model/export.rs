//! Text rendering of a run for `w` (export, M6): header, panels in bpftrace's own text
//! format (`@x[key]: value`, `|@@@@|` histograms), then the event log. Pure.

use std::fmt::Write;

use super::hist;
use super::panels::{Panel, PanelData, display, stats_table, table_rows};
use super::run_state::Run;

/// bpftrace's histogram bar width.
const BAR_WIDTH: usize = 52;

/// `host`: the remote target the run was on (none for local runs).
pub fn render_text(run: &Run, host: Option<&str>, raw_note: Option<&str>) -> String {
    let mut out = String::new();
    let secs = run.elapsed.as_secs();
    let _ = writeln!(out, "bpfdeck run export");
    if let Some(host) = host {
        let _ = writeln!(out, "host:     {host}");
    }
    let _ = writeln!(out, "script:   {}", run.script_id);
    let _ = writeln!(out, "command:  {}", run.command);
    let _ = writeln!(out, "state:    {}", run.state_label());
    let _ = writeln!(
        out,
        "elapsed:  {:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    let probes = run
        .attached_probes
        .map_or_else(|| "-".to_string(), |n| n.to_string());
    let _ = writeln!(
        out,
        "probes:   {probes}   errors: {}   dropped: {}",
        run.errors, run.dropped
    );
    if let Some(note) = raw_note {
        let _ = writeln!(out, "note:     {note}");
    }
    if let Some(edits) = &run.edits {
        let _ = writeln!(
            out,
            "edited:   yes, {} lines vs the source file (changes at the end)",
            edits.summary()
        );
    }

    for panel in &run.panels.list {
        out.push('\n');
        render_panel(&mut out, panel);
    }

    let lines = run.log.matching("");
    let _ = write!(out, "\n--- log ({} lines", lines.len());
    if run.log.evicted() > 0 {
        let _ = write!(out, ", {} older lines evicted", run.log.evicted());
    }
    out.push_str(") ---\n");
    for line in lines {
        out.push_str(&line.display());
        out.push('\n');
    }
    if let Some(edits) = &run.edits {
        let _ = writeln!(out, "\n--- changes vs the source file ({}) ---", edits.summary());
        for change in edits.changes() {
            let _ = writeln!(out, "{change}");
        }
    }
    out
}

fn render_panel(out: &mut String, panel: &Panel) {
    let name = &panel.name;
    match &panel.data {
        PanelData::Hist(series) => {
            for (i, (key, buckets)) in series.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                match key {
                    Some(k) => {
                        let _ = writeln!(out, "{name}[{k}]:");
                    }
                    None => {
                        let _ = writeln!(out, "{name}:");
                    }
                }
                let buckets = hist::trimmed(buckets);
                let max = buckets.iter().map(|b| b.count).max().unwrap_or(0).max(1);
                for (b, label) in buckets.iter().zip(hist::labels(buckets)) {
                    let bar = "@".repeat((b.count as f64 / max as f64 * BAR_WIDTH as f64) as usize);
                    let _ = writeln!(out, "{label:<16}{:>8} |{bar:<BAR_WIDTH$}|", b.count);
                }
            }
        }
        PanelData::Table { rows, .. } => {
            for row in table_rows(rows, None, panel.sort_by_key) {
                let _ = writeln!(out, "{name}[{}]: {}", row.cols.join(", "), row.value);
            }
        }
        PanelData::Value { value, .. } => {
            let _ = writeln!(out, "{name}: {}", display(value));
        }
        PanelData::Stats(value) => {
            let (headers, rows) = stats_table(value);
            let _ = writeln!(out, "{name}: {}", headers.join(" / "));
            for row in rows {
                let _ = writeln!(out, "  {}", row.join("  "));
            }
        }
        PanelData::Tseries(series) => {
            for (key, points) in series {
                match key {
                    Some(k) => {
                        let _ = writeln!(out, "{name}[{k}]:");
                    }
                    None => {
                        let _ = writeln!(out, "{name}:");
                    }
                }
                for p in points {
                    let _ = writeln!(out, "  {}  {}", p.interval_start, p.value);
                }
            }
        }
    }
}

/// `bpfdeck-[<host>-]<script stem>-<UTC yyyymmdd-hhmmss>`, safe as a file name.
pub fn file_stem(host: Option<&str>, script_id: &str, unix_secs: u64) -> String {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let name = script_id.rsplit('/').next().unwrap_or(script_id);
    let name = safe(name.strip_suffix(".bt").unwrap_or(name));
    match host {
        Some(host) => format!("bpfdeck-{}-{name}-{}", safe(host), utc_stamp(unix_secs)),
        None => format!("bpfdeck-{name}-{}", utc_stamp(unix_secs)),
    }
}

/// `yyyymmdd-hhmmss` in UTC, without a date crate (days → civil date, H. Hinnant).
fn utc_stamp(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::bpftrace::json::parse_line;
    use crate::model::run_state::ExitInfo;

    #[test]
    fn stamps_and_stems() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(951_782_400), "20000229-000000", "leap day");
        assert_eq!(utc_stamp(1_790_700_000), "20260929-164000");
        assert_eq!(
            file_stem(None, "net/tcp connect.bt", 0),
            "bpfdeck-tcp_connect-19700101-000000"
        );
        assert_eq!(
            file_stem(None, "shebang_no_ext", 0),
            "bpfdeck-shebang_no_ext-19700101-000000"
        );
        assert_eq!(
            file_stem(Some("ops@10.0.3.14:2222"), "tools/biolatency.bt", 0),
            "bpfdeck-ops_10.0.3.14_2222-biolatency-19700101-000000"
        );
    }

    #[test]
    fn render_a_finished_run() {
        let t0 = Instant::now();
        let mut run = Run::new(
            1,
            "vfs_latency_demo.bt",
            "bpftrace -f json -B line -- /s/vfs_latency_demo.bt",
        );
        run.started(t0);
        let fixtures = [
            "session_mixed.ndjson",
            "hist_multiple.ndjson",
            "stats.ndjson",
            "tseries.ndjson",
        ];
        for name in fixtures {
            let text = std::fs::read_to_string(format!(
                "{}/tests/fixtures/json/{name}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .expect("fixture");
            for msg in text.lines().flat_map(parse_line) {
                run.output(msg, t0);
            }
        }
        for msg in parse_line(r#"{"type":"map","data":{"@x":42}}"#) {
            run.output(msg, t0);
        }
        run.stderr("WARNING: something");
        run.stopping();
        run.exited(
            ExitInfo {
                code: Some(0),
                signal: None,
                forced: None,
                error: None,
            },
            t0 + Duration::from_secs(75),
        );
        insta::assert_snapshot!(render_text(&run, None, Some("raw NDJSON truncated at 256 MiB")));
        run.edits = Some(crate::model::diff::diff("a\nb\n", "a\nx\n"));
        let edited = render_text(&run, None, None);
        assert!(
            edited.contains("edited:   yes, +1 −1 lines vs the source file"),
            "{edited}"
        );
        assert!(
            edited.ends_with("--- changes vs the source file (+1 −1) ---\n-   2 b\n+   2 x\n"),
            "{edited}"
        );
        run.edits = None;
        let remote = render_text(&run, Some("ops@db-02"), None);
        assert!(
            remote.starts_with("bpfdeck run export\nhost:     ops@db-02\nscript:   "),
            "{remote}"
        );
    }
}
