# Architecture

## Shape

One binary crate, modules with hard boundaries. Pure logic (discovery, metadata,
JSON parsing, histogram math) has **no** ratatui or tokio imports so it can be unit
tested without a terminal or a kernel.

```
src/
  main.rs            CLI (clap), runtime bootstrap, terminal init/restore, top-level loop
  app.rs             App state + reducer: fn update(&mut self, Msg) -> Vec<Cmd>
  msg.rs             Msg (input from the world) and Cmd (side effects to perform)
  keymap.rs          Key → Action table per context; also feeds the help modal
  list.rs            `--list` debug output: discovery + metadata as a plain table
  source/            resolve <path|git-url> → local root dir
    mod.rs
    git.rs           git subprocess (clone/fetch), cache dir layout
  discovery/         pure
    mod.rs           walk tree → Vec<ScriptFile>
    metadata.rs      header, usage, probes, params, unsafe hints
    lexer.rs         comment/string stripping, top-level probe spec extraction
  bpftrace/          everything that execs bpftrace
    mod.rs           BpftraceInfo { path, version, supports_dry_run, … } via capability detection
    validate.rs      dry-run / probe-list strategies, worker pool, cache
    runner.rs        spawn, stream stdout/stderr → RunEvent, signals, timeouts
    json.rs          pure: &str → OutputMsg (see docs/bpftrace-json.md)
    command.rs       pure: build argv for run/validate (unit-test every case)
  model/             pure view-models derived from events
    run_state.rs     per-run panels: hist/table/stats/tseries/log ring buffer
    hist.rs          bucket labels, scaling, keyed series
  ui/
    mod.rs           draw(frame, &App) — routes to screens
    theme.rs         Gruvbox palette + semantic styles (ONLY place with colors)
    browser.rs       list + detail tabs
    source_view.rs   line numbers + highlighter
    run_view.rs      header + panel layout
    widgets/         hist.rs, table.rs, log.rs, sparkline.rs, modal.rs, form.rs, filter.rs
  sys.rs             privilege check, lockdown detection, kernel release (uname)
```

## Concurrency model

Elm-style loop, single owner of state:

```
            ┌──────────── terminal input thread (crossterm::event::read) ───┐
            │                                                               │
            ▼                                                               │
   mpsc<Msg> (bounded) ◄── validation workers ◄── tokio tasks              │
            │           ◄── runner stdout/stderr readers                    │
            │           ◄── tick (250 ms while a run is active)             │
            ▼                                                               │
   App::update(msg) -> Vec<Cmd> ──► executor spawns tokio work ──► Msg ─────┘
            │
            ▼
   terminal.draw(|f| ui::draw(f, &app))   (only when something changed)
```

- `tokio` multi-thread runtime; UI loop runs in `main` via `block_on`.
- Terminal input on a dedicated std thread doing blocking `event::read()` and
  forwarding to the channel (avoids needing crossterm's `event-stream` feature).
- `App` never awaits and never does I/O. `Cmd`s describe I/O
  (`Cmd::Validate(ScriptId)`, `Cmd::StartRun{…}`, `Cmd::Signal(RunId, SIGINT)`,
  `Cmd::OpenEditor(path)`), executed by a small executor in `main.rs`.
- This makes `App::update` fully testable: feed `Msg`s, assert state and emitted `Cmd`s.

## Child process handling

- `tokio::process::Command` with `.process_group(0)`, `.kill_on_drop(true)`,
  `.stdin(Stdio::null())`, piped stdout/stderr.
- Stop = `nix::sys::signal::kill(Pid::from_raw(-pgid), SIGINT)` (group), then
  escalate on timeouts (spec §5.4).
- bpfdeck installs handlers for SIGTERM/SIGHUP → stop child, restore terminal, exit.
- bpfdeck's own Ctrl-C while in raw mode arrives as a key event, not SIGINT —
  handle it in the keymap.

## Rendering

- ratatui 0.30 (`ratatui::init()` / `ratatui::restore()`, panic hook included).
- Custom widgets implement `Widget` for `&T` view-model types from `model/`.
- Histogram: horizontal bars with Unicode eighth-blocks (`▏▎▍▌▋▊▉█`) for sub-cell
  precision; label column right-aligned, count column right-aligned.
- Snapshot tests: `ratatui::backend::TestBackend` + `insta::assert_snapshot!` on the
  buffer, fixed sizes 80×24 and 120×40.

## Error handling

- `anyhow` at the edges (`main`, executor), `thiserror` enums in `source`, `bpftrace`.
- Errors in background work become `Msg::…Failed { reason }` and show in the UI.
  Nothing background-related may `unwrap()` or panic.

## Testing without a kernel

- Everything in `discovery/`, `bpftrace/json.rs`, `bpftrace/command.rs`, `model/`
  is pure → unit tests over `tests/fixtures/`.
- `bpftrace/runner.rs` is tested against a **fake bpftrace**: a small shell script in
  `tests/fake_bpftrace/` that replays an `.ndjson` fixture with delays, handles SIGINT
  by printing a final `hist` line, and exits. Point `--bpftrace` at it.
- Real-kernel tests are `#[ignore]` and run manually with `sudo -E cargo test -- --ignored`.
