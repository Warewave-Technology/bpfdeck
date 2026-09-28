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
      Tools: `sudo bpfdeck --list tests/fixtures/scripts` (validation column + details) and
      `sudo bpfdeck --run vfs_latency_demo.bt tests/fixtures/scripts` (Ctrl-C → exit dump).

## M3 — Browser screen
- [ ] Msg/Cmd loop, input thread, executor (architecture.md)
- [ ] Script list with status glyphs, live validation updates
- [ ] Detail tabs: Info, Source (line numbers + highlighter), Validation
- [ ] `/` fuzzy filter, `r` rescan, `e` $EDITOR, `?` help from keymap table
- [ ] Status bar: bpftrace version, kernel, privilege badge; lockdown banner
- [ ] Snapshot tests 80×24 and 120×40

## M4 — Run: confirm, params, log
- [ ] Confirmation modal with exact argv and probe list; unsafe toggle + banner
- [ ] Params form from metadata (positional + getopt, bool checkboxes, defaults)
- [ ] Run view header (state, elapsed, probes, error count, dropped count)
- [ ] Event log panel: ring buffer, follow/pause, filter
- [ ] Stop flow captures the exit-time dump
- [ ] Test with `vfs_latency_demo.bt` and `tcpconnect_demo.bt` on a real host

## M5 — Visualization panels
- [ ] Histogram widget (eighth-block bars, bpftrace-style labels, under/overflow buckets, keyed selector)
- [ ] Top-table widget (sorted, tuple keys split, delta markers, inline bars)
- [ ] Scalar value, stats table, tseries sparkline
- [ ] Panel layout + focus cycling
- [ ] Coalescing under load: run a printf-flood script, UI stays responsive
- [ ] Snapshot tests per widget from fixtures

## M6 — Polish (optional, pick by value)
- [ ] Export current run (NDJSON raw + text rendering) to a file
- [ ] Tree view for script list
- [ ] Privilege separation (D-005 v2)
- [ ] Static musl build + GitHub release workflow (x86_64, aarch64)
- [ ] Rename (D-011)
