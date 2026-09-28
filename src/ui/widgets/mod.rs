//! Panel widgets for the run view (spec §5.4). Each renders one `model::panels::Panel`.

pub mod hist;
pub mod stats;
pub mod table;
pub mod tseries;
pub mod value;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::theme::Theme;
use crate::model::panels::{Panel, PanelData};

pub fn render(frame: &mut Frame, area: Rect, panel: &Panel) {
    match &panel.data {
        PanelData::Hist(_) => hist::render(frame, area, panel),
        PanelData::Table { .. } => table::render(frame, area, panel),
        PanelData::Value { .. } => value::render(frame, area, panel),
        PanelData::Stats(_) => stats::render(frame, area, panel),
        PanelData::Tseries(_) => tseries::render(frame, area, panel),
    }
}

/// `key: curl  (2/5)  [ ] switch` for keyed hists/tseries; `None` when not keyed.
fn key_line(panel: &Panel) -> Option<Line<'static>> {
    let keys = panel.keys();
    let key = keys.get(panel.key_index())?;
    Some(Line::from(vec![
        Span::styled("key ", Theme::label()),
        Span::styled(key.to_string(), Theme::title()),
        Span::styled(
            format!("  ({}/{})  ", panel.key_index() + 1, keys.len()),
            Theme::muted(),
        ),
        Span::styled("[ ]", Theme::key_hint()),
        Span::styled(" switch", Theme::muted()),
    ]))
}

/// Pad or cut `s` to exactly `width` display columns.
fn fit(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    let len = out.chars().count();
    if len < s.chars().count() && width > 0 {
        out.pop();
        out.push('…');
    }
    out + &" ".repeat(width.saturating_sub(len))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::bpftrace::json::parse_line;
    use crate::model::panels::Panels;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/json/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture")
    }

    /// Feed NDJSON into panels (one second per line) and render the last-touched panel.
    fn panels(ndjson: &str) -> Panels {
        let mut panels = Panels::default();
        for (i, line) in ndjson.lines().enumerate() {
            for msg in parse_line(line) {
                panels.apply(&msg, Duration::from_secs(i as u64 + 1));
            }
        }
        panels
    }

    fn draw(panel: &Panel, width: u16, height: u16) -> TestBackend {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal.draw(|f| render(f, f.area(), panel)).expect("draw");
        terminal.backend().clone()
    }

    #[test]
    fn hist_log2() {
        let p = panels(&fixture("hist.ndjson"));
        insta::assert_snapshot!(draw(&p.list[0], 60, 12));
    }

    #[test]
    fn hist_session_usecs() {
        let p = panels(&fixture("session_mixed.ndjson"));
        insta::assert_snapshot!(draw(&p.list[1], 70, 12));
    }

    #[test]
    fn lhist_with_overflow() {
        let p = panels(&fixture("lhist.ndjson"));
        insta::assert_snapshot!(draw(&p.list[0], 60, 14));
    }

    #[test]
    fn hist_keyed_selector() {
        let mut p = panels(&fixture("hist_multiple.ndjson"));
        insta::assert_snapshot!("hist_keyed_first", draw(&p.list[0], 60, 10));
        p.list[0].cycle_key(1);
        insta::assert_snapshot!("hist_keyed_second", draw(&p.list[0], 60, 10));
    }

    #[test]
    fn hist_empty_and_too_short() {
        let p = panels(&fixture("hist_zero.ndjson"));
        insta::assert_snapshot!("hist_empty", draw(&p.list[0], 40, 3));
        let p = panels(&fixture("hist.ndjson"));
        insta::assert_snapshot!("hist_truncated", draw(&p.list[0], 50, 5));
    }

    #[test]
    fn table_with_deltas() {
        let p = panels(&fixture("session_mixed.ndjson"));
        insta::assert_snapshot!(draw(&p.list[0], 60, 7));
    }

    #[test]
    fn table_sorted_by_key() {
        let mut p = panels(&fixture("session_mixed.ndjson"));
        p.list[0].sort_by_key = true;
        insta::assert_snapshot!(draw(&p.list[0], 60, 7));
    }

    #[test]
    fn table_tuple_keys() {
        let p =
            panels(r#"{"type": "map", "data": {"@complex": {"bpftrace,2": 5, "sshd,17": 12, "curl,3": 1}}}"#);
        insta::assert_snapshot!(draw(&p.list[0], 50, 5));
    }

    #[test]
    fn table_narrow_and_truncated() {
        let rows: Vec<String> = (0..30)
            .map(|i| format!("\"a_rather_long_process_name_{i}\": {}", 1000 - i * 30))
            .collect();
        let line = format!(r#"{{"type": "map", "data": {{"@m": {{{}}}}}}}"#, rows.join(", "));
        let p = panels(&line);
        insta::assert_snapshot!(draw(&p.list[0], 40, 8));
    }

    #[test]
    fn scalar_value() {
        let p = panels(
            "{\"type\": \"map\", \"data\": {\"@x\": 41}}\n{\"type\": \"map\", \"data\": {\"@x\": 42}}",
        );
        insta::assert_snapshot!(draw(&p.list[0], 40, 7));
    }

    #[test]
    fn stats_tables() {
        let p = panels(&fixture("stats.ndjson"));
        insta::assert_snapshot!("stats_single", draw(&p.list[0], 40, 3));
        let p = panels(
            r#"{"type": "stats", "data": {"@lat": {"read": {"count": 10, "average": 1234, "total": 12340}, "write": {"count": 2, "average": 50, "total": 100}}}}"#,
        );
        insta::assert_snapshot!("stats_keyed", draw(&p.list[0], 50, 4));
    }

    #[test]
    fn tseries_sparkline() {
        let points: Vec<String> = (0..40)
            .map(|i| {
                format!(
                    r#"{{"interval_start": "12:00:{i:02}", "value": {}}}"#,
                    (i * 7) % 23
                )
            })
            .collect();
        let p = panels(&format!(
            r#"{{"type": "tseries", "data": {{"@rate": [{}]}}}}"#,
            points.join(", ")
        ));
        insta::assert_snapshot!("tseries_sparkline", draw(&p.list[0], 50, 7));
        let p = panels(&fixture("tseries.ndjson"));
        insta::assert_snapshot!("tseries_flat", draw(&p.list[0], 50, 5));
    }
}
