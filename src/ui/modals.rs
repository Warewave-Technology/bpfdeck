//! Modals of the run flow: parameters form (§5.3), run confirmation (§5.2), questions.

use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use super::theme::Theme;
use crate::app::{
    App, Ask, Confirm, ConnectField, ConnectForm, ConnectPhase, ConnectRowState as RowState, SudoMode,
    ValidationState,
};
use crate::bpftrace::command;
use crate::bpftrace::validate::Verdict;
use crate::model::form::{FieldKind, ParamForm};
use crate::remote::connect::{Check, CheckStatus};
use crate::sys::Privilege;

const LABEL: usize = 10;

/// Centered box of at most `width`×`height`, inside `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    rect
}

/// Draw `lines` in a titled popup sized to fit them (wrapped at the popup width).
fn popup(frame: &mut Frame, area: Rect, title: String, lines: Vec<Line<'static>>, max_width: u16) {
    let width = max_width.min(area.width.saturating_sub(4)).max(20);
    let inner_width = usize::from(width.saturating_sub(2)).max(1);
    let rows: usize = lines.iter().map(|l| l.width().div_ceil(inner_width).max(1)).sum();
    let height = u16::try_from(rows + 2).unwrap_or(u16::MAX);
    let rect = centered(area, width, height);
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Theme::border_focused())
        .style(Theme::popup_bg())
        .title(Span::styled(title, Theme::title()));
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(block),
        rect,
    );
}

fn field(label: &str, value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:<LABEL$}"), Theme::label())];
    spans.extend(value);
    Line::from(spans)
}

pub fn draw_form(frame: &mut Frame, area: Rect, script_id: &str, form: &ParamForm) {
    let mut lines: Vec<Line> = form
        .usage
        .iter()
        .map(|u| Line::styled(u.clone(), Theme::muted()))
        .collect();
    if form.uses_argc {
        lines.push(Line::styled(
            "The script reads $# (number of positional arguments).",
            Theme::muted(),
        ));
    }
    if !lines.is_empty() {
        lines.push(Line::raw(""));
    }
    for (i, f) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        let marker = Span::styled(if focused { "› " } else { "  " }, Theme::key_hint());
        let label = Span::styled(
            format!("{:<14}", f.label()),
            if focused { Theme::title() } else { Theme::label() },
        );
        let value = match f.kind {
            FieldKind::Flag { .. } => {
                let text = if f.checked { "[x]" } else { "[ ]" };
                Span::styled(text, if focused { Theme::selected() } else { Theme::base() })
            }
            _ if focused => Span::styled(format!("{}█", f.value), Theme::selected()),
            _ if f.value.is_empty() => Span::styled("(empty)", Theme::muted()),
            _ => Span::raw(f.value.clone()),
        };
        let mut spans = vec![marker, label, value];
        if let Some(help) = &f.help {
            spans.push(Span::styled(format!("  {help}"), Theme::muted()));
        }
        lines.push(Line::from(spans));
    }
    if let Some(e) = &form.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(e.clone(), Theme::error()));
    }
    popup(frame, area, format!(" Parameters · {script_id} "), lines, 90);
}

pub fn draw_confirm(frame: &mut Frame, area: Rect, app: &App, confirm: &Confirm) {
    let bpftrace = app.bpftrace_path().unwrap_or(Path::new("bpftrace"));
    let command_line = match confirm.argv(bpftrace, app.target().is_remote()) {
        Ok(argv) => command::display(&argv),
        Err(e) => format!("(invalid: {e})"),
    };
    let t = app.target();
    let mut lines = Vec::new();
    if !confirm.targets.is_empty() {
        lines.extend(target_checklist(confirm));
        lines.push(field(
            "",
            vec![Span::styled(
                "hosts get a temporary copy of the script",
                Theme::muted(),
            )],
        ));
    } else if t.is_remote() {
        let privilege = t.remote.as_ref().map_or("root", |r| r.privilege.as_str());
        lines.push(field(
            "Target",
            vec![Span::styled(
                format!("{} as {privilege}", t.label),
                Theme::accent(),
            )],
        ));
        lines.push(field(
            "",
            vec![Span::styled(
                "the script is copied there for this run and removed afterwards",
                Theme::muted(),
            )],
        ));
    }
    lines.extend([
        field("Path", path_spans(app, confirm)),
        field("Command", vec![Span::styled(command_line, Theme::code_var())]),
        field(
            "Probes",
            vec![Span::raw(if confirm.probes.is_empty() {
                "(none found)".to_string()
            } else {
                confirm.probes.join(", ")
            })],
        ),
    ]);

    if confirm.needs_unsafe {
        let calls = if confirm.unsafe_calls.is_empty() {
            "unsafe builtins".to_string()
        } else {
            confirm
                .unsafe_calls
                .iter()
                .map(|c| format!("{c}()"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!(" Needs --unsafe: calls {calls}, which act on the system as root. "),
            Theme::banner_error(),
        ));
        lines.push(if confirm.allow_unsafe {
            Line::styled(
                " --unsafe is ON for this run (u to turn off) ",
                Theme::banner_error(),
            )
        } else {
            Line::from(vec![
                Span::styled(" --unsafe is OFF", Theme::warn()),
                Span::styled(": bpftrace will refuse to run it. Press ", Theme::muted()),
                Span::styled("u", Theme::key_hint()),
                Span::styled(" to enable it for this run.", Theme::muted()),
            ])
        });
    }

    let warnings = run_warnings(app, &confirm.script_id);
    if !warnings.is_empty() {
        lines.push(Line::raw(""));
        lines.extend(warnings);
    }
    if let Some(e) = &confirm.error {
        lines.push(Line::raw(""));
        lines.push(Line::styled(e.clone(), Theme::error()));
    }
    let checked = confirm.targets.iter().filter(|c| c.checked).count();
    if !confirm.targets.is_empty() {
        lines.push(Line::raw(""));
        let mut hint = Vec::new();
        for (i, (key, what)) in [
            (
                "Enter",
                format!("run on {checked} target{}", if checked == 1 { "" } else { "s" }),
            ),
            ("Space", "toggle".to_string()),
            ("a", "all that validated".to_string()),
            ("Esc", "cancel".to_string()),
        ]
        .into_iter()
        .enumerate()
        {
            if i > 0 {
                hint.push(Span::styled(" · ", Theme::muted()));
            }
            hint.push(Span::styled(key, Theme::key_hint()));
            hint.push(Span::raw(format!(" {what}")));
        }
        lines.push(Line::from(hint));
    }
    let title = if checked > 1 {
        format!(" Run {} on {checked} targets ", confirm.script_id)
    } else if t.is_remote() {
        format!(" Run {} on {} ", confirm.script_id, t.label)
    } else {
        format!(" Run {} ", confirm.script_id)
    };
    popup(frame, area, title, lines, 100);
}

/// `Targets  › [x] db-01  ● runs (dry-run)` rows of the fleet checklist (F3).
fn target_checklist(confirm: &Confirm) -> Vec<Line<'static>> {
    let width = confirm
        .targets
        .iter()
        .map(|c| c.label.chars().count())
        .max()
        .unwrap_or(0);
    confirm
        .targets
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let here = i == confirm.cursor;
            let (boxed, box_style) = match (&c.unavailable, c.checked) {
                (Some(_), _) => ("[-]", Theme::muted()),
                (None, true) => ("[x]", Theme::ok()),
                (None, false) => ("[ ]", Theme::base()),
            };
            let (g, g_style) = super::browser::glyph(&c.validation);
            let (text, text_style) = match (&c.unavailable, &c.validation) {
                (Some(why), _) => (why.clone(), Theme::muted()),
                (None, ValidationState::Done(v)) => match &v.verdict {
                    Verdict::Ok => ("runs".to_string(), Theme::base()),
                    Verdict::NeedsUnsafe => ("needs --unsafe".to_string(), Theme::warn()),
                    Verdict::Partial { found, total } => {
                        (format!("only {found}/{total} probes found"), Theme::warn())
                    }
                    Verdict::Failed { reason } => (format!("validation failed: {reason}"), Theme::error()),
                },
                (None, ValidationState::Pending) => ("validating…".to_string(), Theme::muted()),
                (None, ValidationState::Skipped(r)) => (r.clone(), Theme::muted()),
            };
            let mut spans = vec![
                Span::styled(
                    if i == 0 {
                        format!("{:<LABEL$}", "Targets")
                    } else {
                        " ".repeat(LABEL)
                    },
                    Theme::label(),
                ),
                Span::styled(if here { "› " } else { "  " }, Theme::key_hint()),
                Span::styled(format!("{boxed} "), box_style),
                Span::styled(
                    format!("{:<width$}  ", c.label),
                    if here { Theme::title() } else { Theme::base() },
                ),
            ];
            if c.unavailable.is_none() {
                spans.push(Span::styled(format!("{g} "), g_style));
            }
            spans.push(Span::styled(text, text_style));
            Line::from(spans)
        })
        .collect()
}

/// The script's path; for an edited script the original path, marked as edited.
fn path_spans(app: &App, confirm: &Confirm) -> Vec<Span<'static>> {
    let original = app.entries.iter().find(|e| e.id() == confirm.script_id);
    match original {
        Some(e) if confirm.edited => vec![
            Span::raw(e.script.file.path.display().to_string()),
            Span::styled(
                format!(
                    "  ✎ your edited version ({} lines)",
                    e.draft.as_ref().map(|d| d.diff.summary()).unwrap_or_default()
                ),
                Theme::accent(),
            ),
        ],
        _ => vec![Span::raw(confirm.path.display().to_string())],
    }
}

/// Why the run may fail here (spec §7): privileges, lockdown, validation result.
fn run_warnings(app: &App, script_id: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if let Some(host) = &app.target().host {
        if host.privilege == Privilege::None {
            out.push(Line::styled(
                "! Not running as root: bpftrace will fail to load and attach (start bpfdeck with sudo).",
                Theme::warn(),
            ));
        }
        if host.lockdown.blocks_bpftrace() {
            out.push(Line::styled(
                "✗ Kernel lockdown is active: bpftrace cannot load programs.",
                Theme::error(),
            ));
        }
    }
    if let Some(entry) = app.entries.iter().find(|e| e.id() == script_id)
        && let ValidationState::Done(v) = app.validation_of(entry)
    {
        match &v.verdict {
            Verdict::Failed { reason } => out.push(Line::styled(
                format!("✗ Validation failed: {reason}"),
                Theme::error(),
            )),
            Verdict::Partial { found, total } => out.push(Line::styled(
                format!("◐ Validation: only {found}/{total} probes found on this kernel."),
                Theme::warn(),
            )),
            Verdict::Ok | Verdict::NeedsUnsafe => {}
        }
    }
    out
}

pub fn draw_ask(frame: &mut Frame, area: Rect, ask: &Ask) {
    let lines = vec![
        Line::raw(ask.question()),
        Line::raw(""),
        Line::from(vec![
            Span::styled("y", Theme::key_hint()),
            Span::raw(" yes   "),
            Span::styled("n", Theme::key_hint()),
            Span::raw(" no"),
        ]),
    ];
    popup(frame, area, " Confirm ".into(), lines, 64);
}

pub fn draw_connect(frame: &mut Frame, area: Rect, form: &ConnectForm) {
    let phase = form.phase();
    let editing = phase != ConnectPhase::Checking;
    let several = form.rows.len() > 1 || form.host_count().is_some_and(|n| n > 1);
    let row = |field: ConnectField, label: &str, value: Vec<Span<'static>>, help: &str| {
        let focused = editing && form.focus == field;
        let mut spans = vec![
            Span::styled(if focused { "› " } else { "  " }, Theme::key_hint()),
            Span::styled(
                format!("{label:<10}"),
                if focused { Theme::title() } else { Theme::label() },
            ),
        ];
        spans.extend(value);
        if !help.is_empty() {
            spans.push(Span::styled(format!("  {help}"), Theme::muted()));
        }
        Line::from(spans)
    };
    let text = |field: ConnectField, value: String, empty: &str| {
        if editing && form.focus == field {
            vec![Span::styled(format!("{value}█"), Theme::selected())]
        } else if value.is_empty() {
            vec![Span::styled(empty.to_string(), Theme::muted())]
        } else {
            vec![Span::raw(value)]
        }
    };
    let mut radios = Vec::new();
    for mode in SudoMode::ALL {
        let on = form.sudo == mode;
        let style = if on && editing && form.focus == ConnectField::Sudo {
            Theme::selected()
        } else if on {
            Theme::title()
        } else {
            Theme::base()
        };
        radios.push(Span::styled(
            format!("({}) {}", if on { "•" } else { " " }, mode.label()),
            style,
        ));
        radios.push(Span::raw("   "));
    }
    let mut lines = vec![
        row(
            ConnectField::Host,
            "Host",
            text(ConnectField::Host, form.host.clone(), ""),
            &match form.host_count() {
                Some(n) if n > 1 => format!("{n} hosts"),
                _ if form.host.is_empty() => {
                    "IP, name, user@host or ssh alias; several: a b, db-0{1..4}".into()
                }
                _ => String::new(),
            },
        ),
        row(
            ConnectField::Port,
            "Port",
            text(ConnectField::Port, form.port.clone(), "default"),
            "",
        ),
        row(ConnectField::Sudo, "sudo", radios, ""),
    ];
    if form.sudo == SudoMode::Password {
        lines.push(row(
            ConnectField::Password,
            "Password",
            text(ConnectField::Password, "•".repeat(form.password.len()), ""),
            "kept in memory while connected",
        ));
    }
    lines.push(row(
        ConnectField::Bpftrace,
        "bpftrace",
        text(
            ConnectField::Bpftrace,
            form.bpftrace.clone(),
            "found in root's PATH",
        ),
        "",
    ));

    let check_line = |check: &Check, indent: &str| {
        let (glyph, style) = match check.status {
            CheckStatus::Ok => ("✓", Theme::ok()),
            CheckStatus::Warn => ("!", Theme::warn()),
            CheckStatus::Fail => ("✗", Theme::error()),
        };
        Line::from(vec![
            Span::styled(format!("{indent}{glyph} "), style),
            Span::styled(
                check.text.clone(),
                if check.status == CheckStatus::Fail {
                    Theme::error()
                } else {
                    Theme::base()
                },
            ),
        ])
    };
    if !form.rows.is_empty() {
        lines.push(Line::raw(""));
    }
    match form.rows.as_slice() {
        // One host: every check as it finishes.
        [row] => {
            lines.extend(row.checks.iter().map(|c| check_line(c, "  ")));
            if row.state == RowState::Checking {
                lines.push(Line::styled("  … checking", Theme::running()));
            }
        }
        // Several hosts: one line each (docs/design-fleet.md).
        rows => {
            let width = rows.iter().map(|r| r.label().chars().count()).max().unwrap_or(0);
            for r in rows {
                let (glyph, style, text, text_style) = match &r.state {
                    RowState::Checking => (
                        "…",
                        Theme::running(),
                        r.checks.last().map_or("connecting".into(), |c| c.text.clone()),
                        Theme::muted(),
                    ),
                    RowState::Connected(summary) => ("✓", Theme::ok(), summary.clone(), Theme::base()),
                    RowState::Failed => (
                        "✗",
                        Theme::error(),
                        r.failure().map_or("failed".into(), |c| c.text.clone()),
                        Theme::error(),
                    ),
                    RowState::NeedsAuth(m) => {
                        ("!", Theme::warn(), format!("ssh needs you: {m}"), Theme::warn())
                    }
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {glyph} "), style),
                    Span::styled(format!("{:<width$}  ", r.label()), Theme::title()),
                    Span::styled(text, text_style),
                ]));
            }
        }
    }
    let hint = |pairs: &[(&str, &str)]| {
        let mut spans = vec![Span::raw("  ")];
        for (i, (key, what)) in pairs.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", Theme::muted()));
            }
            spans.push(Span::styled(key.to_string(), Theme::key_hint()));
            spans.push(Span::raw(format!(" {what}")));
        }
        Line::from(spans)
    };
    let connected = form
        .rows
        .iter()
        .filter(|r| matches!(r.state, RowState::Connected(_)))
        .count();
    let close = if connected > 0 {
        "close (connected hosts stay)"
    } else {
        "cancel"
    };
    match &phase {
        ConnectPhase::Checking => {
            lines.push(Line::raw(""));
            lines.push(hint(&[("Esc", close)]));
        }
        ConnectPhase::NeedsAuth(message) => {
            let waiting = form
                .rows
                .iter()
                .filter(|r| matches!(r.state, RowState::NeedsAuth(_)))
                .count();
            lines.push(Line::raw(""));
            if form.rows.len() == 1 {
                lines.push(Line::styled(
                    format!("  ! ssh needs you: {message}"),
                    Theme::warn(),
                ));
            } else {
                lines.push(Line::styled(
                    format!(
                        "  ! ssh needs you for {waiting} of these hosts; they are done one after the other"
                    ),
                    Theme::warn(),
                ));
            }
            lines.push(Line::styled(
                "    bpfdeck suspends and runs ssh in the terminal, where it can ask for a \
                 passphrase, password or host key; bpfdeck never sees them.",
                Theme::muted(),
            ));
            lines.push(Line::raw(""));
            lines.push(hint(&[("Enter", "authenticate in the terminal"), ("Esc", close)]));
        }
        ConnectPhase::Editing => {
            if let Some(e) = &form.error {
                lines.push(Line::raw(""));
                lines.push(Line::styled(format!("  {e}"), Theme::error()));
            }
            lines.push(Line::raw(""));
            let retry = form.rows.iter().any(|r| r.state == RowState::Failed);
            lines.push(hint(&[
                ("Enter", if retry { "try again" } else { "connect" }),
                ("Tab", "next field"),
                ("Esc", close),
            ]));
        }
    }
    let title = if several {
        " Connect to hosts "
    } else {
        " Connect to a host "
    };
    popup(frame, area, title.into(), lines, 110);
}
