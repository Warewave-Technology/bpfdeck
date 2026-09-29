//! The fleet run's compare tab (docs/design-fleet.md, F4): the hosts, then every map of
//! the run across hosts, one section per map.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::theme::Theme;
use crate::app::App;
use crate::model::compare::{HistRow, KeyRow, Section, ValueRow};
use crate::model::hist;

/// Width of the bars of a merged histogram.
const BAR: usize = 40;

fn num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
    }
}

fn pad(s: &str, w: usize) -> String {
    format!("{s:<w$}")
}

fn rpad(s: &str, w: usize) -> String {
    format!("{s:>w$}")
}

fn outlier(on: bool) -> Span<'static> {
    Span::styled(if on { " ◀" } else { "" }, Theme::warn())
}

fn host_style(stale: bool) -> Style {
    if stale { Theme::muted() } else { Theme::base() }
}

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let cmp = app.comparison();
    let view = &app.compare_view;
    let hw = cmp
        .hosts
        .iter()
        .map(|h| h.label.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let mut lines = vec![Line::styled(
        format!(
            "  {}  {:<22} {:>8}  {:>7}  {:>7}",
            pad("host", hw),
            "state",
            "elapsed",
            "errors",
            "dropped"
        ),
        Theme::label(),
    )];
    for (i, h) in cmp.hosts.iter().enumerate() {
        let s = h.elapsed.as_secs();
        let state_style = if h.active { Theme::running() } else { Theme::base() };
        lines.push(Line::from(vec![
            Span::styled(if i == view.cursor { "› " } else { "  " }, Theme::key_hint()),
            Span::styled(
                pad(&h.label, hw),
                if i == view.cursor {
                    Theme::title()
                } else {
                    Theme::base()
                },
            ),
            Span::raw("  "),
            Span::styled(format!("{:<22}", h.state), state_style),
            Span::raw(format!(" {:>8}", format!("{:02}:{:02}", s / 60, s % 60))),
            Span::styled(
                format!("  {:>7}", h.errors),
                if h.errors > 0 {
                    Theme::error()
                } else {
                    Theme::muted()
                },
            ),
            Span::styled(
                format!("  {:>7}", h.dropped),
                if h.dropped > 0 {
                    Theme::warn()
                } else {
                    Theme::muted()
                },
            ),
        ]));
    }
    if cmp.sections.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("No maps printed yet on any host.", Theme::muted()));
    }
    let host_names: Vec<String> = cmp.hosts.iter().map(|h| h.label.clone()).collect();
    for section in &cmp.sections {
        lines.push(Line::raw(""));
        match section {
            Section::Hist {
                name,
                keyed,
                rows,
                merged,
            } => {
                let what = match (view.merged, keyed) {
                    (true, _) => "merged over hosts (m: per host)",
                    (false, true) => "all keys · p50 / p90 / p99 · max (m: merged)",
                    (false, false) => "p50 / p90 / p99 · max (m: merged)",
                };
                lines.push(title(name, "hist", what));
                if view.merged {
                    lines.extend(merged_lines(merged));
                } else {
                    lines.extend(rows.iter().map(|r| hist_row(r, hw)));
                }
            }
            Section::Table {
                name,
                hosts,
                rows,
                more,
                stale,
            } => {
                let by = match app.compare_view.sort {
                    Some(i) => format!("by {} (s: next)", host_names.get(i).map_or("?", String::as_str)),
                    None => "by total (s: by host)".to_string(),
                };
                lines.push(title(name, "table", &format!("top {} {by}", rows.len())));
                lines.extend(table_lines(hosts, rows, stale, *more));
            }
            Section::Values { name, rows } => {
                lines.push(title(name, "value", ""));
                lines.extend(rows.iter().map(|r| value_row(r, hw)));
            }
            Section::Other { name, kind } => {
                lines.push(title(name, kind, ""));
                lines.push(Line::styled(
                    "  shown per host only: open a host's tab (Enter)",
                    Theme::muted(),
                ));
            }
        }
    }
    // Clamp the scroll to the content.
    let max = u16::try_from(lines.len().saturating_sub(usize::from(area.height))).unwrap_or(u16::MAX);
    let scroll = view.scroll.get().min(max);
    view.scroll.set(scroll);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
}

fn title(name: &str, kind: &str, what: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(name.to_string(), Theme::title()),
        Span::styled(format!(" · {kind}"), Theme::muted()),
        Span::styled(
            if what.is_empty() {
                String::new()
            } else {
                format!("  {what}")
            },
            Theme::muted(),
        ),
    ])
}

fn hist_row(r: &HistRow, hw: usize) -> Line<'static> {
    let host = Span::styled(format!("  {}  ", pad(&r.host, hw)), host_style(r.stale));
    if r.missing {
        return Line::from(vec![host, Span::styled("no data yet", Theme::muted())]);
    }
    let q = |label: &str, v: &Option<String>| format!("{label} {:<14}", v.as_deref().unwrap_or("-"));
    Line::from(vec![
        host,
        Span::styled(format!("count {:<9}", r.count), host_style(r.stale)),
        Span::styled(
            q("p50", &r.p50),
            if r.p50_outlier {
                Theme::warn()
            } else {
                host_style(r.stale)
            },
        ),
        Span::styled(q("p90", &r.p90), host_style(r.stale)),
        Span::styled(
            q("p99", &r.p99),
            if r.p99_outlier {
                Theme::warn()
            } else {
                host_style(r.stale)
            },
        ),
        Span::styled(format!("max {}", r.max.as_deref().unwrap_or("-")), Theme::muted()),
        outlier(r.outlier()),
    ])
}

fn merged_lines(buckets: &[crate::bpftrace::json::Bucket]) -> Vec<Line<'static>> {
    let shown = hist::trimmed(buckets);
    if shown.is_empty() {
        return vec![Line::styled("  (no counts yet)", Theme::muted())];
    }
    let labels = hist::labels(shown);
    let lw = labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let max = shown.iter().map(|b| b.count).max().unwrap_or(0);
    let cw = max.to_string().len();
    shown
        .iter()
        .zip(labels)
        .map(|(b, label)| {
            Line::from(vec![
                Span::styled(format!("  {} ", rpad(&label, lw)), Theme::muted()),
                Span::raw(format!("{} ", rpad(&b.count.to_string(), cw))),
                Span::styled(hist::bar(b.count, max, BAR), Theme::hist_bar()),
            ])
        })
        .collect()
}

fn table_lines(hosts: &[String], rows: &[KeyRow], stale: &[bool], more: usize) -> Vec<Line<'static>> {
    let kw = rows
        .iter()
        .map(|r| r.key.chars().count())
        .max()
        .unwrap_or(3)
        .max(3);
    let cols: Vec<usize> = hosts
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let widest = rows
                .iter()
                .map(|r| r.values[i].map_or(1, |v| num(v).len()))
                .max()
                .unwrap_or(1);
            h.chars().count().max(widest) + 2
        })
        .collect();
    let tw = rows.iter().map(|r| num(r.total).len()).max().unwrap_or(5).max(5);
    let mut header = vec![
        Span::styled(format!("  {}  ", pad("key", kw)), Theme::label()),
        Span::styled(rpad("total", tw), Theme::label()),
    ];
    for (i, h) in hosts.iter().enumerate() {
        header.push(Span::styled(
            format!("{}  ", rpad(h, cols[i])),
            if stale[i] { Theme::muted() } else { Theme::label() },
        ));
    }
    let mut out = vec![Line::from(header)];
    for r in rows {
        let mut spans = vec![
            Span::raw(format!("  {}  ", pad(&r.key, kw))),
            Span::raw(rpad(&num(r.total), tw)),
        ];
        for (i, v) in r.values.iter().enumerate() {
            let text = v.map_or("-".to_string(), num);
            let cell = format!(
                "{}{}",
                rpad(&text, cols[i]),
                if r.outliers[i] { " ◀" } else { "  " }
            );
            let style = if r.outliers[i] {
                Theme::warn()
            } else if stale[i] || v.is_none() {
                Theme::muted()
            } else {
                Theme::base()
            };
            spans.push(Span::styled(cell, style));
        }
        out.push(Line::from(spans));
    }
    if more > 0 {
        out.push(Line::styled(format!("  … {more} more keys"), Theme::muted()));
    }
    out
}

fn value_row(r: &ValueRow, hw: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {}  ", pad(&r.host, hw)), host_style(r.stale)),
        match &r.value {
            Some(v) => Span::styled(
                v.clone(),
                if r.outlier {
                    Theme::warn()
                } else {
                    host_style(r.stale)
                },
            ),
            None => Span::styled("no data yet", Theme::muted()),
        },
        outlier(r.outlier),
    ])
}
