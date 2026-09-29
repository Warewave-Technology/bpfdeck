# Milestones

Each milestone ends with: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` all green, a short demo note in the PR/commit, and checkboxes ticked here.
Do them in order. Do not start a milestone's UI before its pure logic has tests.

## M0 — Skeleton ✅ (done during handoff)
- [x] Cargo project, deps, release profile
- [x] Gruvbox palette + semantic styles in `src/ui/theme.rs`
- [x] ratatui init/restore, placeholder 2-pane layout, `q` quits
- [x] Docs: spec, architecture, JSON format, decisions
- [x] Fixtures: sample `.bt` scripts, NDJSON output samples

## M1 — Source + discovery + metadata (pure, no UI yet) ✅
- [x] `source::resolve(input) -> Result<ResolvedSource>` for local dir, single file, git URL (+`#ref`)
- [x] Cache dir via `directories::ProjectDirs`; clone/update per spec §6.1 (hooks off)
- [x] `discovery::walk(root)` per spec §6.2 — test on `tests/fixtures/scripts`
      (must find 8 scripts incl. `net/tcpconnect_demo.bt` and `shebang_no_ext`, ignore `README.txt`)
- [x] `metadata::extract(&str)`: description, usage, probes, params, unsafe hints — table tests
      for every fixture + multi-line probe lists + predicates + comments containing `{`
- [x] No-panic test: random bytes / truncated scripts
- [x] Temporary `--list` CLI flag printing discovery+metadata as a table (debug aid; keep it)

Demo: `cargo run -- --list tests/fixtures/scripts` (8 scripts) and
`cargo run -- --list https://github.com/bpftrace/bpftrace` (93 scripts incl. `tools/`,
correct descriptions, `opensnoop.bt` → `--depth=35 --errname`). Git sync is tested against
local `file://` repos with the real `git` binary (hooks-disabled check has a positive control).

## M2 — bpftrace integration (pure parts + fake bpftrace)
- [x] `bpftrace::json::parse_line` for every fixture in `tests/fixtures/json/`
- [x] `bpftrace::command` argv builder: positional vs `--` named params, unsafe flag, validate modes (D-013)
- [x] Capability detection: version, `--dry-run` support (from `--help`), privileges, lockdown
- [x] `tests/fake_bpftrace/fake-bpftrace.sh` replaying fixtures, handling SIGINT with a final dump
- [x] Runner: spawn, stream, SIGINT→TERM→KILL escalation, exit status — tested with the fake
- [x] Validation worker pool + cache — tested with the fake (`--dry-run` success/failure paths)
- [ ] **Manual on real hosts**: capture `real_*` fixtures (see docs/bpftrace-json.md checklist)
      Partly done on a Docker VM kernel (D-016): Debian 13 / bpftrace 0.23.2 fixtures and
      findings in docs/real-kernel-testing.md. RHEL 8/9 and Debian 12 still open.
      Tools: `sudo bpfdeck --list tests/fixtures/scripts` (validation column + details) and
      `sudo bpfdeck --run vfs_latency_demo.bt tests/fixtures/scripts` (Ctrl-C → exit dump).

## M3 — Browser screen ✅
- [x] Msg/Cmd loop, input thread, executor (architecture.md)
- [x] Script list with status glyphs, live validation updates
- [x] Detail tabs: Info, Source (line numbers + highlighter), Validation
- [x] `/` fuzzy filter, `r` rescan, `e` $EDITOR, `?` help from keymap table
- [x] Status bar: bpftrace version, kernel, privilege badge; lockdown banner
- [x] Snapshot tests 80×24 and 120×40

Demo (driven in tmux against the fake bpftrace): list validates live via `-l` without
root, `G` `2` shows highlighted source, `/tcpc` filters, `e` runs `$EDITOR` and the edit
shows up after the automatic rescan, `?` opens/closes help, `q` and SIGTERM both exit 0
with the terminal restored and no leftover processes.

## M4 — Run: confirm, params, log
- [x] Confirmation modal with exact argv and probe list; unsafe toggle + banner
- [x] Params form from metadata (positional + getopt, bool checkboxes, defaults) (D-014: before the confirmation)
- [x] Run view header (state, elapsed, probes, error count, dropped count)
- [x] Event log panel: ring buffer, follow/pause, filter
- [x] Stop flow captures the exit-time dump
- [ ] Test with `vfs_latency_demo.bt` and `tcpconnect_demo.bt` on a real host
      Run/stop/export verified on the Docker VM kernel (D-016) with a tracepoint read-latency
      script, since `kprobe:vfs_read` does not fire there. A real host is still needed.

Demo (tmux, fake bpftrace): Enter → confirmation → run; the log streams a replayed
session, `x` captures the exit-time `hist` and shows `exited(0)`; the params form sends
`-- params_demo.bt 1234 --verbose` (checked in the fake's argv log); `q` during a run asks,
stops it and exits 0 with no leftover processes.

## M5 — Visualization panels ✅
- [x] Histogram widget (eighth-block bars, bpftrace-style labels, under/overflow buckets, keyed selector)
- [x] Top-table widget (sorted, tuple keys split, delta markers, inline bars)
- [x] Scalar value, stats table, tseries sparkline
- [x] Panel layout + focus cycling
- [x] Coalescing under load: run a printf-flood script, UI stays responsive
- [x] Snapshot tests per widget from fixtures

Demo (tmux, fake bpftrace): a replayed session shows the `@syscalls` table with ↑/↓/+ and
the `@usecs` histogram (Tab); `x` adds the exit-time `@final` panel. A 10M-line printf
flood (`// fake: flood=`) keeps key latency at 10–20 ms with nothing dropped; the unit
test `tui::tests::flood_is_coalesced_and_accounted_for` checks accounting under a slow
consumer (delivered + dropped = sent, snapshots coalesced, exit last).

## M8 — Fleet mode (docs/design-fleet.md, D-024)
- [x] F1 Host list in the connect dialog (parse + expand), parallel checks, per-host rows
- [x] F2 Validation matrix in the Validation tab, disagreement count in the list
- [ ] F3 Target checklist in the confirmation, `FleetRun`, start/stop/quit on the group
- [ ] F4 `model/compare.rs` + compare tab: hist percentiles/merge, key × host tables, outliers
- [ ] F5 Fleet export, spec/README/D-entries, end-to-end with 3 sshd containers

## M7 — Remote targets over SSH (docs/design-remote.md) ✅
- [x] R1 Targets in the app model; bottom results pane with one tab per target (D-021)
- [x] R2 Embedded runner + session protocol; SSH backend for capture/run; `tests/fake_ssh`
- [x] R3 Connect dialog and checks, terminal handoff for interactive auth, master lifecycle, `d`
- [x] R4 Validation on connect, host in confirmation and exports, lost connection + reconnect
- [x] R5 Real end-to-end against sshd containers (root, NOPASSWD, password sudo;
      `tests/realhost/sshd.sh`), docs; fixes it found are in D-022
- [x] (added) `d` shown as a hint on remote tabs; inline editing with session drafts (D-025)

## M6 — Polish (optional, pick by value)
- [x] Export current run (NDJSON raw + text rendering) to a file (`w`, spec §6.6)
- [x] Tree view for script list (D-019)
- [x] ~~Privilege separation (D-005 v2)~~ — dropped: root is the model (D-018)
- [x] Static musl build + GitHub release workflow (x86_64, aarch64) (D-015; not pushed yet)
- [x] Rename (D-011) — the name stays `bpfdeck` (D-017)
- [x] (added) Real-kernel harness `tests/realhost/` (D-016) and the fixes it led to:
      unsafe wording of bpftrace 0.23, repeated-error collapsing, empty-bucket collapsing
