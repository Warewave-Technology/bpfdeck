# Architecture

## Shape

One binary crate, modules with hard boundaries. Pure logic (discovery, metadata,
JSON parsing, histogram math) has **no** ratatui or tokio imports so it can be unit
tested without a terminal or a kernel.

```
src/
  main.rs            CLI (clap), runtime bootstrap, dispatch to tui / headless
  tui.rs             terminal init/restore, input thread, signals, Msg loop, executor
  app/               App state + reducer: fn update(&mut self, Msg) -> Vec<Cmd>
    mod.rs
    filter.rs        fuzzy filter (nucleo-matcher)
  msg.rs             Msg (input from the world) and Cmd (side effects to perform)
  catalog.rs         resolve + discover + read + extract metadata (blocking)
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
    run_state.rs     one run: phase, counters, log, latest snapshot per map
    log.rs           event log ring buffer (line joining, filter, eviction)
    form.rs          parameters form state → positional + named args
    describe.rs      one-line text for any OutputMsg (log summaries, --run)
    hist.rs          bucket labels, scaling, keyed series (M5)
  ui/
    mod.rs           draw(frame, &App) — routes to screens
    theme.rs         Gruvbox palette + semantic styles (ONLY place with colors)
    browser.rs       list + detail tabs
    help.rs          `?` modal, generated from keymap.rs
    source_view.rs   line numbers + highlighter (reuses discovery::lexer regions)
    run_view.rs      header + log (panel layout in M5)
    modals.rs        params form, run confirmation, yes/no question
    widgets/         hist.rs, table.rs, log.rs, sparkline.rs, modal.rs, form.rs, filter.rs
  sys.rs             privilege check, lockdown detection, kernel release (/proc)
  headless.rs        `--list` / `--run <ID>`: debug entry points without the TUI
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
  `Cmd::OpenEditor(path)`), executed by a small executor in `tui.rs`.
- This makes `App::update` fully testable: feed `Msg`s, assert state and emitted `Cmd`s.
- After each `Msg` the loop drains everything already queued, then draws once.
- `$EDITOR` handoff: the input thread does every `poll`+`read` while holding a mutex
  ("input gate"); the executor takes the gate, restores the terminal, runs the editor
  (`block_in_place`), re-enters raw mode/alternate screen, then releases the gate. So no
  keystroke meant for the editor is read by bpfdeck.
- The validator is created by the `DetectEnv` task and published through a `OnceLock`;
  the app only emits `Cmd::Validate` after `Msg::EnvDetected`, so it is always set.
- UI state that depends on the pane size (detail scroll clamping) lives in a `Cell` on
  `App`, written back by the renderer; everything else is plain reducer state.

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
- `bpftrace/runner.rs` and `validate.rs` are tested against a **fake bpftrace**: a POSIX
  sh script in `tests/fake_bpftrace/` that answers `--version`/`--help`/`-l`/`--dry-run`,
  replays an `.ndjson` fixture with delays, handles SIGINT by printing a final `hist`
  line, and exits. Tests drive it with `// fake: key=value` directives inside the test
  script (see the header of `fake-bpftrace.sh`), not env vars, so parallel tests stay
  isolated. `fake-bpftrace-old.sh` is an old version without `--dry-run`. Point
  `--bpftrace` at either.
- Real-kernel tests are `#[ignore]` and run manually with `sudo -E cargo test -- --ignored`.
