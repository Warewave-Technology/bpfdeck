//! Text rendering of a run for `w` (export, M6): header, panels in bpftrace's own text
//! format (`@x[key]: value`, `|@@@@|` histograms), then the event log. Pure.

use std::fmt::Write;

use super::compare::{Comparison, OUTLIER_FACTOR, Section};
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

/// The comparison report of a fleet run (F5): hosts, then each map across hosts, as in
/// the compare tab but untruncated. `cmp` should be built with no key limit.
pub fn render_fleet_text(script_id: &str, cmp: &Comparison) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "bpfdeck fleet run comparison");
    let _ = writeln!(out, "script:   {script_id}");
    let _ = writeln!(out, "hosts:    {}", cmp.hosts.len());
    let _ = writeln!(
        out,
        "outliers: ◀ marks a value above {OUTLIER_FACTOR}× the median of the other hosts (hists: p50 or p99)"
    );
    let hw = cmp
        .hosts
        .iter()
        .map(|h| h.label.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let _ = writeln!(
        out,
        "\n{:<hw$}  {:<24} {:>8} {:>7} {:>7}",
        "host", "state", "elapsed", "errors", "dropped"
    );
    for h in &cmp.hosts {
        let s = h.elapsed.as_secs();
        let _ = writeln!(
            out,
            "{:<hw$}  {:<24} {:>8} {:>7} {:>7}",
            h.label,
            h.state,
            format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60),
            h.errors,
            h.dropped
        );
    }
    let mark = |on: bool| if on { " ◀" } else { "" };
    for section in &cmp.sections {
        out.push('\n');
        match section {
            Section::Hist {
                name,
                keyed,
                rows,
                merged,
            } => {
                let keys = if *keyed { " (all keys summed)" } else { "" };
                let _ = writeln!(out, "{name}: hist{keys}");
                for r in rows {
                    if r.missing {
                        let _ = writeln!(out, "  {:<hw$}  no data", r.host);
                        continue;
                    }
                    let q = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
                    let _ = writeln!(
                        out,
                        "  {:<hw$}  count {:<10} p50 {:<14} p90 {:<14} p99 {:<14} max {}{}",
                        r.host,
                        r.count,
                        q(&r.p50),
                        q(&r.p90),
                        q(&r.p99),
                        q(&r.max),
                        mark(r.outlier())
                    );
                }
                let _ = writeln!(out, "  merged over the hosts:");
                let buckets = hist::trimmed(merged);
                let max = buckets.iter().map(|b| b.count).max().unwrap_or(0).max(1);
                for (b, label) in buckets.iter().zip(hist::labels(buckets)) {
                    let bar = "@".repeat((b.count as f64 / max as f64 * BAR_WIDTH as f64) as usize);
                    let _ = writeln!(out, "  {label:<16}{:>8} |{bar:<BAR_WIDTH$}|", b.count);
                }
            }
            Section::Table {
                name,
                hosts,
                rows,
                more: _,
                stale: _,
            } => {
                let _ = writeln!(out, "{name}: map, by total");
                let kw = rows
                    .iter()
                    .map(|r| r.key.chars().count())
                    .max()
                    .unwrap_or(3)
                    .max(3);
                let mut header = format!("  {:<kw$}  {:>12}", "key", "total");
                for h in hosts {
                    let _ = write!(header, "  {h:>14}");
                }
                let _ = writeln!(out, "{header}");
                for r in rows {
                    let mut line = format!("  {:<kw$}  {:>12}", r.key, fmt_num(r.total));
                    for (v, o) in r.values.iter().zip(&r.outliers) {
                        let cell = format!("{}{}", v.map_or("-".into(), fmt_num), mark(*o));
                        let _ = write!(line, "  {cell:>14}");
                    }
                    let _ = writeln!(out, "{}", line.trim_end());
                }
            }
            Section::Values { name, rows } => {
                let _ = writeln!(out, "{name}: value");
                for r in rows {
                    let v = r.value.clone().unwrap_or_else(|| "no data".into());
                    let _ = writeln!(out, "  {:<hw$}  {v}{}", r.host, mark(r.outlier));
                }
            }
            Section::Other { name, kind } => {
                let _ = writeln!(out, "{name}: {kind} (see each host's export)");
            }
        }
    }
    out
}

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
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
    fn fleet_report() {
        use crate::model::compare::{Member, compare};
        let t0 = Instant::now();
        let run_with = |lines: &[String]| {
            let mut run = Run::new(1, "biolatency.bt", "bpftrace");
            run.started(t0);
            for msg in lines.iter().flat_map(|l| parse_line(l)) {
                run.output(msg, t0 + Duration::from_secs(42));
            }
            run.tick(t0 + Duration::from_secs(42));
            run
        };
        let hist = |slow: u64| {
            format!(
                r#"{{"type": "hist", "data": {{"@usecs": [{{"min": 16, "max": 31, "count": 40}}, {{"min": 32, "max": 63, "count": 50}}, {{"min": 8192, "max": 16383, "count": {slow}}}]}}}}"#
            )
        };
        let map =
            |p: u64| format!(r#"{{"type": "map", "data": {{"@calls": {{"postgres": {p}, "sshd": 3}}}}}}"#);
        let a = run_with(&[hist(1), map(1207)]);
        let b = run_with(&[hist(900), map(8205)]);
        let c = run_with(&[hist(2), map(1100)]);
        let members = [
            Member {
                label: "db-01",
                run: &a,
            },
            Member {
                label: "db-02",
                run: &b,
            },
            Member {
                label: "db-03",
                run: &c,
            },
        ];
        insta::assert_snapshot!(render_fleet_text(
            "tools/biolatency.bt",
            &compare(&members, None, usize::MAX)
        ));
    }

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
