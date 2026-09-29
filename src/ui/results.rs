//! Bottom pane: one results tab per target (docs/design-remote.md). The selected tab is the
//! active target; its run view (header, panels, log) fills the pane.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph, Wrap};

use super::theme::Theme;
use super::{compare_view, run_view};
use crate::app::target::{Conn, Target};
use crate::app::{App, BpftraceState, Screen};
use crate::model::run_state::Phase;
use crate::sys::Privilege;

/// `▶` running, `✓`/`✗` last run finished, `✗` connection lost.
fn tab_glyph(t: &Target) -> Option<(&'static str, Style)> {
    if matches!(t.conn, Conn::Lost(_)) {
        return Some(("✗", Theme::error()));
    }
    let run = t.run.as_ref()?;
    Some(match run.phase {
        Phase::Starting | Phase::Running | Phase::Stopping => ("▶", Theme::running()),
        Phase::Exited if run.succeeded() => ("✓", Theme::ok()),
        Phase::Exited | Phase::Failed => ("✗", Theme::error()),
    })
}

fn tab_bar(app: &App) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    // The fleet run's compare tab comes first (F4).
    if let Some(fleet) = &app.fleet {
        let name = fleet.script_id.rsplit('/').next().unwrap_or(&fleet.script_id);
        let name = name.strip_suffix(".bt").unwrap_or(name);
        let label = format!("⧉ {name}");
        let selected = app.compare_selected();
        spans.push(Span::styled(
            if selected { format!("[{label}]") } else { label },
            if selected {
                Theme::tab_active()
            } else {
                Theme::tab_inactive()
            },
        ));
        if app.fleet_members().iter().any(|(_, r)| r.is_active()) {
            spans.push(Span::styled(" ▶", Theme::running()));
        }
        spans.push(Span::styled(" │ ", Theme::border()));
    }
    for (i, t) in app.targets.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" │ ", Theme::border()));
        }
        let active = i == app.active && !app.compare_selected();
        // A target that cannot run anything must stand out, on or off the selected tab.
        let broken = matches!(t.bpftrace, BpftraceState::Missing(_)) || matches!(t.conn, Conn::Lost(_));
        let style = match (broken, active) {
            (true, _) => Theme::tab_broken(),
            (false, true) => Theme::tab_active(),
            (false, false) => Theme::tab_inactive(),
        };
        let mut label = t.label.clone();
        if matches!(t.bpftrace, BpftraceState::Missing(_)) {
            label.push_str(" · no bpftrace");
        }
        spans.push(Span::styled(
            if active { format!("[{label}]") } else { label },
            style,
        ));
        if let Some((g, s)) = tab_glyph(t) {
            spans.push(Span::styled(format!(" {g}"), s));
        }
    }
    spans.push(Span::styled(" │ ", Theme::border()));
    spans.push(Span::styled("+ ", Theme::key_hint()));
    Line::from(spans)
}

/// `root via sudo · 5.14.0-427… · Rocky Linux 9.4` for the right side of the title.
/// Fits into `width` columns by dropping parts from the end (the tabs come first).
fn target_summary(t: &Target, width: usize) -> String {
    let mut parts = Vec::new();
    match (&t.remote, &t.host) {
        (Some(r), _) => parts.push(r.privilege.clone()),
        (None, Some(h)) => parts.push(
            match h.privilege {
                Privilege::Root => "root",
                Privilege::Caps => "caps",
                Privilege::None => "NO PRIV",
            }
            .to_string(),
        ),
        _ => {}
    }
    if let Some(h) = &t.host {
        parts.push(h.kernel_release.clone());
    }
    if let Some(r) = &t.remote {
        parts.push(r.os.clone());
    }
    parts.retain(|p| !p.is_empty());
    while !parts.is_empty() {
        let s = format!(" {} ", parts.join(" · "));
        if s.chars().count() <= width {
            return s;
        }
        parts.pop();
    }
    String::new()
}

/// Columns left in the top border after the tab bar (corners and a gap excluded).
fn summary_room(app: &App, area: Rect) -> usize {
    usize::from(area.width).saturating_sub(tab_bar(app).width() + 4)
}

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let t = app.target();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if app.screen == Screen::Run && app.overlay.is_none() {
            Theme::border_focused()
        } else {
            Theme::border()
        })
        .title(tab_bar(app));
    let summary = if app.compare_selected() {
        let n = app.fleet_members().len();
        let s = format!(" fleet run on {n} target{} ", if n == 1 { "" } else { "s" });
        if s.chars().count() <= summary_room(app, area) {
            s
        } else {
            String::new()
        }
    } else {
        target_summary(t, summary_room(app, area))
    };
    let block = block.title(Line::styled(summary, Theme::muted()).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.compare_selected() {
        compare_view::draw(frame, inner, app);
        return;
    }

    if let Some(run) = &t.run {
        run_view::draw(frame, inner, app, run);
        return;
    }
    let mut lines = Vec::new();
    match (&t.conn, &t.bpftrace) {
        (Conn::Lost(reason), _) => lines.push(Line::styled(
            format!(
                "Connection to {} lost: {reason}. c reconnects, d closes the tab.",
                t.label
            ),
            Theme::error(),
        )),
        (_, BpftraceState::Missing(_)) => lines.push(Line::from(vec![
            Span::styled(format!("No bpftrace on {}. ", t.label), Theme::error()),
            Span::styled("c", Theme::key_hint()),
            Span::styled(" connects to a host where scripts can run.", Theme::muted()),
        ])),
        (_, BpftraceState::Detecting) => lines.push(Line::styled("detecting bpftrace…", Theme::muted())),
        _ if t.is_remote() => lines.push(Line::from(vec![
            Span::styled(
                format!("No run on {} yet: select a script and press ", t.label),
                Theme::muted(),
            ),
            Span::styled("Enter", Theme::key_hint()),
            Span::styled(". ", Theme::muted()),
            Span::styled("d", Theme::key_hint()),
            Span::styled(" disconnects.", Theme::muted()),
        ])),
        _ => lines.push(Line::from(vec![
            Span::styled(
                format!("No run on {} yet: select a script and press ", t.label),
                Theme::muted(),
            ),
            Span::styled("Enter", Theme::key_hint()),
            Span::styled(". ", Theme::muted()),
            Span::styled("c", Theme::key_hint()),
            Span::styled(" connects to a host.", Theme::muted()),
        ])),
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
