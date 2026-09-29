//! Browser screen: script list (left) and detail tabs (right). Spec §5.1.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, List, ListItem, ListState, Paragraph, Wrap};

use super::theme::Theme;
use super::{editor_view, source_view};
use crate::app::tree::ListRow;
use crate::app::{App, BpftraceState, Entry, LoadState, Screen, TABS, ValidationState};
use crate::bpftrace::validate::{Strategy, Validation, Verdict};

/// Width of the field labels in the Info and Validation tabs.
const LABEL_WIDTH: usize = 9;

/// Status glyph and style for a script (spec §5.1 table).
pub fn glyph(state: &ValidationState) -> (&'static str, Style) {
    match state {
        ValidationState::Pending => ("…", Theme::muted()),
        ValidationState::Skipped(_) => ("?", Theme::muted()),
        ValidationState::Done(v) => match v.verdict {
            Verdict::Ok => ("●", Theme::ok()),
            Verdict::Partial { .. } => ("◐", Theme::warn()),
            Verdict::NeedsUnsafe => ("!", Theme::warn()),
            Verdict::Failed { .. } => ("✗", Theme::error()),
        },
    }
}

pub fn draw_list(frame: &mut Frame, area: Rect, app: &App) {
    let focused = app.overlay.is_none() && app.screen == Screen::Browser;
    let count = if app.load != LoadState::Ready {
        " Scripts ".to_string()
    } else if app.query.is_empty() {
        format!(" Scripts ({}) ", app.entries.len())
    } else {
        format!(" Scripts ({}/{}) ", app.visible.len(), app.entries.len())
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Theme::border_focused()
        } else {
            Theme::border()
        })
        .title(Span::styled(count, Theme::title()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let show_filter = app.filter_editing || !app.query.is_empty();
    let [body, filter] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(u16::from(show_filter))]).areas(inner);

    match &app.load {
        LoadState::Loading(msg) => {
            let text = Paragraph::new(Line::styled(msg.as_str(), Theme::muted())).wrap(Wrap { trim: false });
            frame.render_widget(text, body);
        }
        LoadState::Failed(e) => {
            let text = vec![
                Line::styled("Cannot load the source:", Theme::error()),
                Line::raw(e.as_str()),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("r", Theme::key_hint()),
                    Span::styled(" retry", Theme::muted()),
                ]),
            ];
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), body);
        }
        LoadState::Ready if app.entries.is_empty() => {
            let root = app
                .source
                .as_ref()
                .map(|s| s.root.display().to_string())
                .unwrap_or_default();
            let text = vec![
                Line::styled("No bpftrace scripts found in", Theme::muted()),
                Line::raw(root),
            ];
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), body);
        }
        LoadState::Ready if app.visible.is_empty() => {
            frame.render_widget(Paragraph::new(Line::styled("no matches", Theme::muted())), body);
        }
        LoadState::Ready => {
            let running = app.target().active_run().map(|r| r.script_id.as_str());
            let tree = app.showing_tree();
            let items: Vec<ListItem> = app
                .rows
                .iter()
                .map(|row| match row {
                    ListRow::Script { entry, depth } => {
                        let e = &app.entries[*entry];
                        list_row(
                            e,
                            app.validation_of(e),
                            running == Some(e.id()),
                            tree.then_some(*depth),
                        )
                    }
                    ListRow::Dir {
                        path,
                        depth,
                        expanded,
                        scripts,
                    } => dir_row(path, *depth, *expanded, *scripts),
                })
                .collect();
            let list = List::new(items).highlight_style(Theme::selected());
            let mut state = ListState::default().with_selected(Some(app.cursor));
            frame.render_stateful_widget(list, body, &mut state);
        }
    }

    if show_filter {
        let mut spans = vec![
            Span::styled("/", Theme::key_hint()),
            Span::raw(app.query.as_str()),
        ];
        if app.filter_editing {
            spans.push(Span::styled("█", Theme::key_hint()));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), filter);
    }
}

/// `▾ tools/  45` (or `▸` when collapsed), indented by depth.
fn dir_row(path: &str, depth: usize, expanded: bool, scripts: usize) -> ListItem<'static> {
    let name = path.rsplit('/').next().unwrap_or(path);
    ListItem::new(Line::from(vec![
        Span::raw("  ".repeat(depth)),
        Span::styled(if expanded { "▾ " } else { "▸ " }, Theme::key_hint()),
        Span::styled(format!("{name}/"), Theme::label()),
        Span::styled(format!("  {scripts}"), Theme::muted()),
    ]))
}

/// `● net/tcpconnect_demo  Description.  (reason)`, the directory dimmed; `▶` while running.
/// In tree view (`depth` set) the row is indented and the directory is implied.
fn list_row(
    entry: &Entry,
    state: &ValidationState,
    running: bool,
    depth: Option<usize>,
) -> ListItem<'static> {
    let (g, style) = if running {
        ("▶", Theme::running())
    } else {
        glyph(state)
    };
    let id = entry.id();
    let (dir, name) = match id.rsplit_once('/') {
        Some(_) if depth.is_some() => (String::new(), id.rsplit('/').next().unwrap_or(id)),
        Some((dir, name)) => (format!("{dir}/"), name),
        None => (String::new(), id),
    };
    let name = name.strip_suffix(".bt").unwrap_or(name).to_string();
    let mut spans = vec![
        Span::raw("  ".repeat(depth.unwrap_or(0))),
        Span::styled(g, style),
        Span::raw(" "),
        Span::styled(dir, Theme::muted()),
        Span::raw(name),
    ];
    if let Some(d) = &entry.draft {
        spans.push(Span::styled(
            " ✎",
            if d.active { Theme::accent() } else { Theme::muted() },
        ));
    }
    if let Some(desc) = &entry.shown().meta.description {
        spans.push(Span::styled(format!("  {desc}"), Theme::muted()));
    }
    if let Some(suffix) = row_suffix(state) {
        spans.push(Span::styled(format!("  {suffix}"), style));
    }
    ListItem::new(Line::from(spans))
}

fn row_suffix(state: &ValidationState) -> Option<String> {
    let ValidationState::Done(v) = state else {
        return None;
    };
    match &v.verdict {
        Verdict::Partial { found, total } => Some(format!("({found}/{total} probes)")),
        Verdict::Failed { reason } => Some(reason.clone()),
        _ => None,
    }
}

pub fn draw_detail(frame: &mut Frame, area: Rect, app: &App) {
    let mut tabs = vec![Span::raw(" ")];
    for (i, name) in TABS.iter().enumerate() {
        if i > 0 {
            tabs.push(Span::styled(" │ ", Theme::border()));
        }
        let style = if i == app.tab {
            Theme::tab_active()
        } else {
            Theme::tab_inactive()
        };
        tabs.push(Span::styled(format!("{} {name}", i + 1), style));
    }
    tabs.push(Span::raw(" "));
    let tabs = Line::from(tabs);
    let tabs_width = tabs.width();
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if app.editor.is_some() {
            Theme::border_focused()
        } else {
            Theme::border()
        })
        .title(tabs);
    // The selected ID on the right, when it fits next to the tabs (it is in the list anyway).
    let title_id = match app.selected_row() {
        Some(ListRow::Dir { path, .. }) => Some(format!("{path}/")),
        _ => app.selected().map(|e| e.id().to_string()),
    };
    if let Some(title_id) = title_id {
        let id = if app.editor.is_some() {
            Line::styled(format!(" ✎ editing {title_id} "), Theme::accent()).right_aligned()
        } else {
            Line::styled(format!(" {title_id} "), Theme::muted()).right_aligned()
        };
        if tabs_width + id.width() + 4 <= usize::from(area.width) {
            block = block.title(id);
        }
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(ListRow::Dir { path, scripts, .. }) = app.selected_row() {
        frame.render_widget(
            Paragraph::new(dir_lines(app, path, *scripts)).wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }
    let Some(entry) = app.selected() else {
        let msg = if matches!(app.load, LoadState::Ready) {
            "no script selected"
        } else {
            ""
        };
        frame.render_widget(Paragraph::new(Line::styled(msg, Theme::muted())), inner);
        return;
    };
    let (lines, wrap) = match app.tab {
        0 => (info_lines(entry, app), true),
        1 if app.editor.as_ref().is_some_and(|e| e.id == entry.id()) => {
            if let Some(ed) = &app.editor {
                editor_view::draw(frame, inner, ed);
            }
            return;
        }
        1 => (source_view::lines(&entry.shown().content), false),
        _ => (validation_lines(app, entry), true),
    };

    // Clamp the scroll offset to the content (the app can't know the pane height).
    let height = usize::from(inner.height);
    let width = usize::from(inner.width.max(1));
    let total: usize = if wrap {
        lines.iter().map(|l| l.width().div_ceil(width).max(1)).sum()
    } else {
        lines.len()
    };
    let max = u16::try_from(total.saturating_sub(height)).unwrap_or(u16::MAX);
    let scroll = app.scroll.get().min(max);
    app.scroll.set(scroll);

    let mut paragraph = Paragraph::new(lines).scroll((scroll, 0));
    if wrap {
        paragraph = paragraph.wrap(Wrap { trim: false });
    }
    frame.render_widget(paragraph, inner);
}

/// `Label    value…`; continuation lines are indented under the value.
fn field(label: &str, first: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:<LABEL_WIDTH$}"), Theme::label())];
    spans.extend(first);
    Line::from(spans)
}

fn cont(spans: Vec<Span<'static>>) -> Line<'static> {
    let mut all = vec![Span::raw(" ".repeat(LABEL_WIDTH))];
    all.extend(spans);
    Line::from(all)
}

fn fields(label: &str, rows: Vec<Vec<Span<'static>>>) -> Vec<Line<'static>> {
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| if i == 0 { field(label, row) } else { cont(row) })
        .collect()
}

/// Where "runs here" refers to: `None` for local, the host label for a remote target.
fn place(app: &App) -> Option<String> {
    let t = app.target();
    t.is_remote().then(|| t.label.clone())
}

fn status_spans(state: &ValidationState, place: Option<String>) -> Vec<Span<'static>> {
    let (g, style) = glyph(state);
    let text = match state {
        ValidationState::Pending => "validating…".to_string(),
        ValidationState::Skipped(reason) => format!("not validated: {reason}"),
        ValidationState::Done(v) => match &v.verdict {
            Verdict::Ok => place.map_or_else(|| "runs here".to_string(), |p| format!("runs on {p}")),
            Verdict::Partial { found, total } => format!("{found}/{total} probes found"),
            Verdict::NeedsUnsafe => "needs --unsafe".to_string(),
            Verdict::Failed { reason } => format!("cannot run here: {reason}"),
        },
    };
    let mut spans = vec![Span::styled(format!("{g} {text}"), style)];
    if let ValidationState::Done(v) = state {
        spans.push(Span::styled(
            format!(" ({})", strategy_name(v.strategy)),
            Theme::muted(),
        ));
    }
    spans
}

fn strategy_name(s: Strategy) -> &'static str {
    match s {
        Strategy::DryRun => "dry-run",
        Strategy::ProbeList => "probe listing, heuristic",
    }
}

fn info_lines(entry: &Entry, app: &App) -> Vec<Line<'static>> {
    let meta = &entry.shown().meta;
    let mut lines = vec![
        match &meta.description {
            Some(d) => Line::raw(d.clone()),
            None => Line::styled("(no description)", Theme::muted()),
        },
        Line::raw(""),
        field("Status", status_spans(app.validation_of(entry), place(app))),
        field(
            "File",
            vec![
                Span::raw(entry.script.file.path.display().to_string()),
                Span::styled(format!("  ({} bytes)", entry.script.file.size), Theme::muted()),
            ],
        ),
    ];
    match &entry.draft {
        Some(d) if d.active => lines.push(field(
            "Edited",
            vec![Span::styled(
                "✎ runs use your version, the file is unchanged (u: original)",
                Theme::accent(),
            )],
        )),
        Some(_) => lines.push(field(
            "Edited",
            vec![Span::styled(
                "showing the original (u: your edits)",
                Theme::muted(),
            )],
        )),
        None => {}
    }

    if !meta.usage.is_empty() {
        lines.extend(fields(
            "Usage",
            meta.usage.iter().map(|u| vec![Span::raw(u.clone())]).collect(),
        ));
    }

    let probe_rows: Vec<Vec<Span<'static>>> = if meta.probes.is_empty() {
        vec![vec![Span::styled("none found", Theme::muted())]]
    } else {
        let done = match app.validation_of(entry) {
            ValidationState::Done(v) => Some(v),
            _ => None,
        };
        meta.probes
            .iter()
            .map(|p| {
                if p.always_available {
                    return vec![
                        Span::styled("· ", Theme::muted()),
                        Span::raw(p.spec.clone()),
                        Span::styled("  (always available)", Theme::muted()),
                    ];
                }
                let (mark, style) = match done.map(|v| probe_found(v, &p.spec)) {
                    Some(Some(true)) => ("✓ ", Theme::ok()),
                    Some(Some(false)) => ("✗ ", Theme::error()),
                    _ => ("· ", Theme::muted()),
                };
                vec![Span::styled(mark, style), Span::raw(p.spec.clone())]
            })
            .collect()
    };
    lines.extend(fields("Probes", probe_rows));

    let params = &meta.params;
    let mut param_rows: Vec<Vec<Span<'static>>> = params
        .positional
        .iter()
        .map(|n| {
            vec![
                Span::styled(format!("${n}"), Theme::code_var()),
                Span::styled("  positional", Theme::muted()),
            ]
        })
        .collect();
    for p in &params.named {
        let text = match (&p.default, p.is_bool) {
            (Some(d), false) => format!("--{}={d}", p.name),
            _ => format!("--{}", p.name),
        };
        let mut row = vec![Span::styled(text, Theme::code_var())];
        if p.is_bool {
            row.push(Span::styled("  flag", Theme::muted()));
        }
        if let Some(desc) = &p.description {
            row.push(Span::styled(format!("  {desc}"), Theme::muted()));
        }
        param_rows.push(row);
    }
    if params.uses_argc {
        param_rows.push(vec![Span::styled("reads $# (argument count)", Theme::muted())]);
    }
    if param_rows.is_empty() {
        param_rows.push(vec![Span::styled("none", Theme::muted())]);
    }
    lines.extend(fields("Params", param_rows));

    if meta.needs_unsafe() {
        let calls = meta
            .unsafe_calls
            .iter()
            .map(|c| format!("{c}()"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(field(
            "Flags",
            vec![Span::styled(
                format!("! calls {calls}: needs --unsafe"),
                Theme::warn(),
            )],
        ));
    }
    if let BpftraceState::Missing(reason) = &app.target().bpftrace {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("bpftrace unavailable: {reason}"),
            Theme::muted(),
        ));
    }
    lines
}

/// Per-probe result: from the probe listing, implied by a passed dry run, or named in a
/// failed dry run's error output.
fn probe_found(v: &Validation, spec: &str) -> Option<bool> {
    if let Some(check) = v.probes.iter().find(|c| c.probe == spec) {
        return Some(check.found);
    }
    match (v.strategy, &v.verdict) {
        (Strategy::DryRun, Verdict::Ok) => Some(true),
        (Strategy::DryRun, Verdict::Failed { .. }) if v.output.contains(spec) => Some(false),
        _ => None,
    }
}

fn validation_lines(app: &App, entry: &Entry) -> Vec<Line<'static>> {
    let state = app.validation_of(entry);
    let mut lines = vec![field("Result", status_spans(state, place(app)))];
    let ValidationState::Done(v) = state else {
        return lines;
    };
    let how = match v.strategy {
        Strategy::DryRun => "bpftrace --dry-run: parse, load and attach, then exit",
        Strategy::ProbeList => "bpftrace -l per probe (heuristic: the script is not loaded)",
    };
    lines.push(field("Method", vec![Span::raw(how)]));
    if !v.notes.is_empty() {
        lines.extend(fields(
            "Notes",
            v.notes
                .iter()
                .map(|n| vec![Span::raw(format!("- {n}"))])
                .collect(),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("Output", Theme::label()));
    if v.output.trim().is_empty() {
        lines.push(Line::styled("(no output)", Theme::muted()));
    } else {
        lines.extend(v.output.lines().map(|l| Line::raw(l.replace('\t', "    "))));
    }
    lines
}

/// Detail pane for a directory row: how many scripts, and how they validated.
fn dir_lines(app: &App, path: &str, scripts: usize) -> Vec<Line<'static>> {
    let prefix = format!("{path}/");
    let mut counts: Vec<(&'static str, Style, &'static str, usize)> = vec![
        ("●", Theme::ok(), "run here", 0),
        ("◐", Theme::warn(), "partially (probes missing)", 0),
        ("!", Theme::warn(), "need --unsafe", 0),
        ("✗", Theme::error(), "cannot run here", 0),
        ("…", Theme::muted(), "still validating", 0),
        ("?", Theme::muted(), "not validated", 0),
    ];
    for e in app.entries.iter().filter(|e| e.id().starts_with(&prefix)) {
        let (g, _) = glyph(app.validation_of(e));
        if let Some(c) = counts.iter_mut().find(|c| c.0 == g) {
            c.3 += 1;
        }
    }
    let mut lines = vec![
        Line::from(vec![
            Span::styled(prefix.clone(), Theme::title()),
            Span::styled(
                format!("  {scripts} script{}", if scripts == 1 { "" } else { "s" }),
                Theme::muted(),
            ),
        ]),
        Line::raw(""),
    ];
    lines.extend(counts.into_iter().filter(|c| c.3 > 0).map(|(g, style, what, n)| {
        Line::from(vec![Span::styled(format!("{g} {n:>4} "), style), Span::raw(what)])
    }));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("Enter", Theme::key_hint()),
        Span::styled(" open/close  ", Theme::muted()),
        Span::styled("← →", Theme::key_hint()),
        Span::styled(" collapse / expand  ", Theme::muted()),
        Span::styled("t", Theme::key_hint()),
        Span::styled(" flat list", Theme::muted()),
    ]));
    lines
}
