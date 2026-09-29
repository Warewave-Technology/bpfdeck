# CLAUDE.md — bpfdeck

TUI (Rust, ratatui, Gruvbox dark) that takes a directory or git repo, lists the
bpftrace scripts in it, validates them against the running kernel, runs them and
visualizes their `-f json` output live.

## Read first (in this order)
1. `docs/milestones.md` — what to build next. Work top to bottom; tick boxes as you go.
2. `docs/spec.md` — behavior. When the spec and your intuition differ, follow the spec
   or propose a change to it; don't silently diverge.
3. `docs/architecture.md` — module boundaries and the Msg/Cmd loop.
4. `docs/bpftrace-json.md` — before touching `src/bpftrace/json.rs` or any panel.
5. `docs/decisions.md` — settled questions. Add a new `D-0xx` entry for any
   architectural choice you make; never edit old entries, supersede them.

## Owner and working style
- Owner: Yiğit (DevOps/SRE, Go/Rust). Communicates in Turkish or English; code,
  comments, commits and docs are in English.
- He prefers learning by doing: when a design choice is non-obvious, explain the
  trade-off in 2–4 sentences before or after the change, not a lecture.
- Keep this file under ~200 lines. Session notes go in `docs/`, not here.

## Commands
```sh
cargo run -- tests/fixtures/scripts            # local dir
cargo run -- https://github.com/bpftrace/bpftrace   # git source (tools/ inside)
cargo run -- --bpftrace tests/fake_bpftrace/fake-bpftrace.sh tests/fixtures/scripts
cargo run -- --list tests/fixtures/scripts     # headless: discovery + metadata + validation
cargo run -- --run params_demo.bt --param 1234 --param=--verbose tests/fixtures/scripts
sudo -E cargo run -- ...                       # real runs need root (D-005)
cargo test                                     # must pass without root or bpftrace
sudo -E cargo test -- --ignored                # real-kernel tests
tests/realhost/sshd.sh                         # SSH target container for remote tests
cargo fmt && cargo clippy --all-targets -- -D warnings
cargo insta review                             # after intentional UI changes
tests/realhost/run.sh --list tests/fixtures/scripts  # real bpftrace in a privileged container (D-016)
```

## Hard rules
- **No colors outside `src/ui/theme.rs`.** Use the semantic `Theme::*` styles; add a
  new role there if needed.
- **Pure core.** `discovery/`, `bpftrace/json.rs`, `bpftrace/command.rs`, `model/`
  must not import ratatui or tokio.
- **App never does I/O.** `App::update(Msg) -> Vec<Cmd>`; side effects run in the executor.
- **Never invoke a shell.** Build argv vectors; pass params as separate args.
- **Never pass `--unsafe` implicitly** (D-009). Never run git hooks or submodules (D-004).
- **Child process hygiene:** own process group, `kill_on_drop`, SIGINT→SIGTERM→SIGKILL
  escalation, killed on bpfdeck exit/panic/SIGTERM/SIGHUP.
- **Unknown bpftrace output is data, not an error.** Log raw, keep going.
- **No `unwrap()`/`expect()` outside tests and `main` startup.** Background failures
  become `Msg::…Failed` and are shown in the UI.
- Terminal must always be restored (ratatui::init installs the panic hook — keep it).
- Tests must pass on a machine without bpftrace and without root. Anything needing a
  kernel is `#[ignore]` with a comment saying why.

## Conventions
- Rust 2024 edition, `cargo fmt` (max_width 110), clippy clean with `-D warnings`.
- Errors: `thiserror` enums in library-ish modules, `anyhow` in `main`/executor.
- Tests: unit tests next to code (`#[cfg(test)] mod tests`), fixture-driven tests read
  from `tests/fixtures/`, UI snapshots with `insta` on `TestBackend` at 80×24 and 120×40.
- Commits: small, one concern each, imperative subject (`discovery: parse multi-line probe lists`).
- Dependencies: ask before adding any crate not already in `Cargo.toml`
  (candidates already approved if needed: `nucleo-matcher` for fuzzy filter,
  `unicode-width`).

## Gruvbox roles cheat sheet (see theme.rs)
ok=green · warn=yellow · error=red · running=aqua · hist bars=blue ·
accent=purple · key hints=orange · borders=bg3, focused=yellow · base bg0/fg.

## bpftrace facts you'll need
- Output: `-f json -B line` → NDJSON, one `{"type":…}` per line. Errors → stderr, text.
- `hist` type covers both `hist()` and `lhist()`; buckets may miss `min` or `max`.
- On SIGINT bpftrace runs `END` and dumps remaining maps — capture it.
- `--dry-run`: parse + load + attach, then exit. Needs root. Detect support via `--help`.
- `-l '<probe>'` lists matching probes; `-l file.bt` lists a program's probes.
- Params: `bpftrace [opts] -- file.bt [positional…] [--x=val|--x]`. Always put the file and
  all params after `--`: getopt permutes, so values could otherwise become options (D-013).
- Kernel lockdown (Secure Boot) blocks bpftrace entirely → detect and explain.
- Versions differ: 0.23 has no `getopt`, words the unsafe refusal differently, prints no
  `attached_probes.count`. Real scripts emit `helper_error` floods. docs/real-kernel-testing.md.

## Current state
M0 done: skeleton compiles and runs (`q` quits), docs and fixtures in place.
M1 done: `source/` (local + git, D-012), `discovery/` (walk, lexer, metadata), `--list`.
M2 done except the manual real-host step: `bpftrace/` (json, command D-013, detect,
runner, validate), `sys.rs`, fake bpftrace, headless `--list`/`--run`.
M3 done: TUI browser (`tui.rs` loop/executor, `app/` reducer, `keymap.rs`, `ui/`).
M4 done except the real-host test: run flow (form → confirm → run view, D-014), `model/`
(run_state, log, form), executor StartRun/StopRun + 250 ms tick.
M5 done: panels (`model/hist.rs`, `model/panels.rs`, `ui/widgets/`), coalescing forwarder
(`bpftrace/coalesce.rs`), prioritized input channel + frame budget in `tui.rs`.
M6 (partial): export (`w`, spool), release workflow (D-015), real-kernel harness (D-016).
Tree view done (D-019); name final (D-017); root is the model (D-018).
M7 done: remote targets over SSH, agentless (docs/design-remote.md, D-020…D-023):
`remote/`, `app/target.rs`, `app/connect.rs`, `ui/results.rs`, `tests/fake_ssh/`,
`tests/realhost/sshd.sh`. Inline editing with session drafts (D-025).
M8 fleet mode (docs/design-fleet.md, D-024): F1 done (host lists, parallel connect),
F2 done (validation per target, disagreement count), F3 done (target checklist,
`FleetRun`, `X`), F4 done (compare tab, `model/compare.rs`). Next: F5 (fleet export,
end-to-end with 3 sshd containers).
Open: RHEL/Debian 12 host checks. Remote `origin` is set; nothing pushed until release.
