//! Run view (spec §5.4): header line + event log. Panels for maps/hists come in M5.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use super::theme::Theme;
use super::widgets;
use crate::app::App;
use crate::model::log::LogKind;
use crate::model::run_state::{Phase, Run};

pub fn draw(frame: &mut Frame, area: Rect, app: &App, run: &Run) {
    let [header, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    frame.render_widget(Paragraph::new(header_lines(run)), header);
    if run.panels.list.is_empty() {
        draw_log(frame, body, app, run);
        return;
    }
    // One panel at a time (tab bar when several) over the log, spec §5.4.
    let [panel, log] = Layout::vertical([Constraint::Percentage(70), Constraint::Percentage(30)]).areas(body);
    draw_panel(frame, panel, run);
    draw_log(frame, log, app, run);
}

fn draw_panel(frame: &mut Frame, area: Rect, run: &Run) {
    let panels = &run.panels;
    let Some(focused) = panels.focused() else { return };
    let mut title = vec![Span::raw(" ")];
    if panels.list.len() > 1 {
        for (i, p) in panels.list.iter().enumerate() {
            if i > 0 {
                title.push(Span::styled(" │ ", Theme::border()));
            }
            let style = if i == panels.focus {
                Theme::tab_active()
            } else {
                Theme::tab_inactive()
            };
            title.push(Span::styled(p.name.clone(), style));
        }
    } else {
        title.push(Span::styled(focused.name.clone(), Theme::title()));
    }
    title.push(Span::styled(format!(" · {} ", focused.kind()), Theme::muted()));
    let info = format!(" {} updates · {} ", focused.updates, elapsed(focused.updated_at));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Theme::border())
        .title(Line::from(title))
        .title(Line::styled(info, Theme::muted()).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    widgets::render(frame, inner, focused);
}

/// `▶ vfs_latency_demo.bt  running  00:12`
/// `  probes 3  errors 0  dropped 0`
fn header_lines(run: &Run) -> Vec<Line<'static>> {
    let (glyph, style) = phase_style(run);
    let count = |label: &str, n: u64, bad: Style| {
        let style = if n > 0 { bad } else { Theme::muted() };
        vec![
            Span::styled(format!("  {label} "), Theme::muted()),
            Span::styled(n.to_string(), style),
        ]
    };
    let first = Line::from(vec![
        Span::styled(format!("{glyph} "), style),
        Span::styled(run.script_id.clone(), Theme::title()),
        Span::styled(format!("  {}", run.state_label()), style),
        Span::styled(format!("  {}", elapsed(run.elapsed)), Theme::base()),
    ]);
    let mut second = vec![
        Span::styled("  probes ", Theme::muted()),
        Span::raw(
            run.attached_probes
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
        ),
    ];
    second.extend(count("errors", run.errors, Theme::error()));
    second.extend(count("dropped", run.dropped, Theme::warn()));
    vec![first, Line::from(second)]
}

fn phase_style(run: &Run) -> (&'static str, Style) {
    match run.phase {
        Phase::Starting => ("▶", Theme::muted()),
        Phase::Running => ("▶", Theme::running()),
        Phase::Stopping => ("◼", Theme::warn()),
        Phase::Exited if run.succeeded() => ("✓", Theme::ok()),
        Phase::Exited | Phase::Failed => ("✗", Theme::error()),
    }
}

/// `mm:ss`, or `h:mm:ss` past an hour.
pub fn elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

fn kind_style(kind: LogKind) -> Style {
    match kind {
        LogKind::Output => Theme::base(),
        LogKind::Error => Theme::error(),
        LogKind::Raw | LogKind::System => Theme::muted(),
    }
}

fn draw_log(frame: &mut Frame, area: Rect, app: &App, run: &Run) {
    let view = &app.target().log_view;
    let focused = app.overlay.is_none();
    let mode = if view.follow {
        " Log · following "
    } else {
        " Log · paused "
    };
    let lines = run.log.matching(&view.filter);
    let mut info = if view.filter.is_empty() {
        format!(" {} lines", run.log.len())
    } else {
        format!(" {}/{} lines", lines.len(), run.log.len())
    };
    if run.log.evicted() > 0 {
        info.push_str(&format!(" · {} evicted", run.log.evicted()));
    }
    info.push(' ');
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Theme::border_focused()
        } else {
            Theme::border()
        })
        .title(Span::styled(
            mode,
            if view.follow {
                Theme::title()
            } else {
                Theme::warn()
            },
        ))
        .title(Line::styled(info, Theme::muted()).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let show_filter = view.editing || !view.filter.is_empty();
    let [body, filter] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(u16::from(show_filter))]).areas(inner);

    let height = usize::from(body.height);
    let bottom_top = lines.len().saturating_sub(height);
    let start = if view.follow {
        bottom_top
    } else {
        lines
            .iter()
            .position(|l| l.seq >= view.top_seq)
            .unwrap_or(lines.len())
            .min(bottom_top)
    };
    view.visible_top.set(lines.get(start).map(|l| l.seq));
    view.page.set(height.max(1));

    let shown: Vec<Line> = lines
        .iter()
        .skip(start)
        .take(height)
        .map(|l| Line::styled(l.display().replace('\t', "    "), kind_style(l.kind)))
        .collect();
    if shown.is_empty() && !view.filter.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("no matching lines", Theme::muted())),
            body,
        );
    } else {
        frame.render_widget(Paragraph::new(shown), body);
    }

    if show_filter {
        let mut spans = vec![
            Span::styled("/", Theme::key_hint()),
            Span::raw(view.filter.clone()),
        ];
        if view.editing {
            spans.push(Span::styled("█", Theme::key_hint()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), filter);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_format() {
        assert_eq!(elapsed(Duration::from_secs(0)), "00:00");
        assert_eq!(elapsed(Duration::from_secs(75)), "01:15");
        assert_eq!(elapsed(Duration::from_secs(3661)), "1:01:01");
    }
}
