# bpfdeck — product spec

> Working name. Rename freely before M1 is merged (crate name, binary, docs, cache dir).

## 1. One-liner

Point bpfdeck at a directory or a git repository, see every bpftrace script in it,
know **before running** whether each one works on this kernel, run it, and watch its
output as live histograms, top-tables and a filterable event log — in a Gruvbox-dark TUI.

## 2. Users and moment of use

- SRE/DevOps engineer on a Linux box (often over SSH) during an incident or a
  performance investigation.
- They have (or know of) a script collection: `bpftrace/tools`, a team repo, a local
  folder of one-off `.bt` files.
- Today they `ls`, read headers, guess which script fits, run it, fight with raw text
  output, Ctrl-C, scroll back. bpfdeck removes each of those steps.

## 3. Scope

### In scope (v1)
- bpftrace only. Input: `.bt` files and extensionless files whose shebang contains `bpftrace`.
- Source: local path or git URL (`https://…`, `git@…`, `ssh://…`), optional `#ref` suffix
  for branch/tag/commit (`https://github.com/bpftrace/bpftrace#v0.27.0`).
- Linux only. Targets: RHEL 8/9 family and Debian/Ubuntu (see decisions D-008).
- Targets: the host bpfdeck runs on, plus SSH hosts connected from inside the TUI (§6.9).
  Nothing is installed on them (D-020).

### Out of scope (v1) — do not build, do not stub
- BCC Python tools, libbpf/CO-RE binaries, raw BPF object files.
- Fleet mode: one script on several hosts at once (targets are tabs so it can come later).
- Writing edits back to the source (inline edits are session drafts, §6.8, D-025).
- Persisting run history to disk (M6 is optional export only).
- Any network access other than the `git` subprocess for cloning and `ssh` for targets.

## 4. Core flow

```
bpfdeck <path|git-url>
  → resolve source (clone/update into cache if git)
  → discover scripts (walk tree)
  → parse metadata (header comment, probes, params, unsafe calls)
  → validate against this kernel (background, cached per script hash)
  → optionally `c`: connect to SSH hosts, each a results tab, validated there too (§6.9)
  → user browses list, reads source/info (for the selected target tab)
  → user runs a script: params form (if any) → confirm dialog → run view (D-014)
  → live output: hist / map table / stats / log
  → stop (SIGINT → final END output → exit) → output stays viewable
```

## 5. Screens

### 5.1 Browser (default screen)
Two panes + status bar.

Left: script list, grouped by directory: a collapsible tree (default when scripts live in
subdirectories) or a flat list with a dim path prefix (`t` toggles). In the tree,
directory rows show `▾`/`▸`, the name and how many scripts are below; `Enter` or `→`/`l`
opens, `←`/`h` closes or jumps to the parent directory. With more than 30 scripts the
tree starts with its top-level directories closed (an overview on the first screen).
While a filter is active the list is flat and ranked; clearing it returns to the tree,
opening the directories above the selected script. A selected directory shows a summary
(script count per validation status) in the detail pane. Each script row:

```
● biolatency        Block I/O latency as a histogram.
◐ tcpconnect        Trace TCP connect()s.            (2/3 probes)
✗ missing_probe     …                                 kprobe:this_… not found
! unsafe_demo       Uses system(); requires --unsafe
… opensnoop         (validating)
```

Status glyph + color (theme roles in `src/ui/theme.rs`):

| Glyph | Meaning | Style |
|---|---|---|
| `●` | Runs here (dry-run passed, or all probes found) | `Theme::ok` |
| `◐` | Partially: some probes of a multi-attach list missing | `Theme::warn` |
| `✗` | Cannot run here (parse error, no probe matched, needs missing BTF…) | `Theme::error` |
| `!` | Needs `--unsafe` | `Theme::warn` |
| `…` | Validation pending | `Theme::muted` |
| `?` | Not validated (bpftrace not found / not runnable) | `Theme::muted` |
| `▶` | Currently running | `Theme::running` |

Right pane, tabs (`Tab`/`Shift-Tab` or `1..3`):
1. **Info** — description, usage lines from header, probes list with per-probe ✓/✗,
   parameters detected, flags (unsafe, uses `-c`/`-p` semantics hint), file path, size.
2. **Source** — the script with line numbers and light syntax highlighting (comments,
   strings, probe lines, builtins, `@maps`). Hand-written highlighter, no tree-sitter in v1.
   `i` edits the script right here (§6.8).
3. **Validation** — raw stderr of the last dry-run / probe check, verbatim.

Fuzzy filter: `/` opens an input line; matches name + description.

### 5.2 Run confirmation (modal)
Always shown before execution. Contents:
- Script name and full path.
- Exact command line that will run (after params), e.g.
  `bpftrace -f json -B line -- /cache/…/biolatency.bt --interval=5`.
- Probes it will attach to.
- Red banner if `--unsafe` would be required; the `--unsafe` flag is **off** by default and
  must be toggled explicitly in this dialog (`u`), and the banner stays.
- Keys: `Enter` run, `u` toggle unsafe (only if needed), `Esc` cancel.
- Also lists why the run may fail here: no privileges, kernel lockdown, failed or partial
  validation. These are warnings; the run is still allowed.

### 5.3 Parameters form (modal, only if the script uses parameters)
- Positional: every `$1..$N` used in the script → one text field each. `$#` usage noted.
- Named: every `getopt("name")` / `getopt("name", default)` → field prefilled with the default;
  boolean when called with one argument or a bool default → checkbox.
- USAGE lines from the header are shown above the form as help.
- Values are passed as separate argv entries (never through a shell).
- Positional fields cover `$1..$max` (bpftrace params are positional, `$3` needs `$1`,
  `$2`); trailing empty ones are not passed, gaps become empty strings.
- Named values equal to the default are not passed (D-014).
- Keys: `Tab`/`↓` next field, `S-Tab`/`↑` previous, `Space` toggles a checkbox (types a
  space in text fields), `Enter` continue to the confirmation, `Esc` cancel.

### 5.4 Run view
Lives in the results pane at the bottom: one tab per target (`local` first, then connected
hosts; `<` `>` switch, `+` connects). The selected tab is the active target: the list's
status glyphs, the Info/Validation tabs and `Enter` refer to it. The pane is three rows
high until a run exists or it has the focus (`o`), then most of the screen; `z` gives it
the whole screen. Tab glyphs: `▶` running, `✓`/`✗` last run, `✗` connection lost; a
target without bpftrace (or with a lost connection) is drawn red, `local · no bpftrace`.
One run per target; runs on different targets go on together (D-021).

Header (two lines, so it fits next to the list at 80 columns): script name, state
(`starting | running | stopping | exited(code) | killed(sig) | failed`), elapsed time;
then attached probe count (from `attached_probes`), error count, dropped count.

Body is a set of **panels**, one per output stream, created on first appearance:

| bpftrace JSON type | Panel | Rendering |
|---|---|---|
| `hist` (log2 and linear) | Histogram | Horizontal bars, bucket labels like bpftrace (`[16, 32)`), count, bar scaled to max. Keyed hists (`@x[comm] = hist()`) → key selector (`[`/`]`). |
| `map` with object data | Top table | Sorted by value desc, key column + value column + inline bar. Column for tuple keys split on `,`. Shows top N that fits; `s` toggles sort key/value. |
| `map` with scalar data | Value | Big number / string, with last-changed time. |
| `stats` | Stats table | count / average / total per key. |
| `tseries` | Sparkline / line chart | x = interval_start, y = value. |
| `printf`, `time`, `cat`, `join`, `syscall`, `value` | Event log | Append-only, ring buffer (default 10k lines), follow mode, `/` filter, pause (`p`). |
| `helper_error`, stderr lines | Event log (error style) + error counter in header | |
| unknown `type` | Event log, raw JSON, muted | Never crash on unknown types. |

Panel layout: if exactly one non-log panel exists, it takes ~70% height and the log the
rest. With several, cycle focus with `Tab` and show one at a time with a panel tab bar.
Panels appear in order of first message. When the run exits, focus moves to the panel the
exit-time dump updated last (e.g. biolatency's histogram) unless the user already picked
one with `Tab`.

Panel details (M5):
- Histogram labels follow bpftrace's text output: `[n]` single-value buckets, `[a, b)`,
  `(..., 0)` underflow, `[100, ...)` overflow; log2 bounds that are multiples of 1024 use
  K/M/G/T/P/E, lhist only when its step is a multiple of 1024. Leading and trailing empty
  buckets are hidden, gaps in between kept (like bpftrace). Bars use eighth blocks.
- Top table: `+` new key, `↑`/`↓` vs the previous snapshot (none on the first one); sort
  marker `▼` (value) / `▲` (key); numeric keys sort numerically; rows that don't fit are
  counted (`… N more rows`).
- Value: the value centered, with the run time of its last *change* and the update count.
- Stats: `count/average/total` per key when the shape has them, otherwise a generic
  key/value table (also used for tseries shapes we don't recognize).
- Tseries: sparkline of the newest points that fit, with last/min/max and time range.

Map semantics: each new message for a map name **replaces** the previous snapshot
(bpftrace prints the whole map each time, and scripts usually `clear()` after print).
Keep the previous snapshot to show deltas (↑/↓ markers) in the top table.

Stop: `Ctrl-C` or `x` inside run view sends SIGINT to bpftrace (NOT to bpfdeck), which
makes bpftrace run `END` and print remaining maps — those final messages must be
captured and rendered. After 5 s without exit → SIGTERM, after 2 s more → SIGKILL.
Leaving the run view with `Esc` does not stop the run; the list row keeps `▶`. `Enter`
on the running script shows its run view again; `o` shows the last run (running or
finished), whose output stays viewable until the next run starts.

v1: one run at a time. Starting another asks to stop the current one. `q` with an active
run asks, then stops it and quits once it has exited (so the exit-time dump still runs).

Log panel: follows the newest line by default. Scrolling up (`k`, `PgUp`, `g`) or `p`
pauses on the lines currently shown; `G`/`End`, `p`, or scrolling back to the bottom
follows again. `/` filters (case-insensitive substring) without changing pause/follow.
map/hist/stats/tseries messages go to panels only, not to the log.

### 5.5 Status bar
Context-sensitive key hints (`Theme::key_hint` for keys), bpftrace version, kernel
release, privilege status (`root` / `caps` / `NO PRIV` in red).

### 5.6 Help
`?` shows a modal with all keybindings, generated from the same keymap table the
handlers use (single source of truth).

## 6. Behavior details

### 6.1 Source resolution
- Local path: must exist and be a directory or a single script file.
- Git URL: fetch with the `git` binary into
  `$XDG_CACHE_HOME/bpfdeck/repos/<sha256(url)[..16]>/` (`url` without `#ref`): `git init`
  on first use, then always `git fetch --depth 1 -- origin <ref|HEAD>` +
  `git reset --hard FETCH_HEAD` behind a "Updating…" status (D-012). On update failure use
  the cached copy and show a warning. Abbreviated commit hashes as `#ref` → full fetch,
  then resolve locally. Refs that could be read as options are rejected.
- Never run hooks from the cloned repo (`-c core.hooksPath=/dev/null`), never recurse submodules.

### 6.2 Discovery
- Walk recursively, skip `.git`, `target`, `node_modules`, hidden dirs, symlink loops,
  files > 1 MiB. Follow no symlinks outside the root.
- Candidate if `*.bt`, or the first line is a shebang containing `bpftrace`.
- Sort by relative path. Stable IDs = relative path.

### 6.3 Metadata extraction (pure functions, heavily unit-tested)
- **Description**: bpftrace/tools convention — first comment line `// name<TAB/spaces>Description.`
  Take the text after the name. Fallback: first non-empty line of the leading comment block
  (`//` lines or a `/* … */` block; comments after the first code line don't count).
  Fallback: none.
- **Usage**: comment lines starting with `USAGE:` / `Usage:` / `Example of usage:` block.
- **Probes**: lightweight lexer — strip comments and strings, find probe specs that precede
  `{` or a predicate `/…/` at top level (brace depth 0). A probe line may list several
  comma-separated probes and span lines. Keep them as written (wildcards included).
  Special: `BEGIN`, `END`, `begin`, `end`, `interval:`, `profile:`, `software:`,
  `hardware:`, `self:` are "always available" for the probe-list check.
- **Parameters**: regex `\$([0-9]+)` (ignore `$#` but record it) and
  `getopt("name"[, default[, "description"]])` (the description argument exists since
  bpftrace 0.24, e.g. `tools/opensnoop.bt`). `$N` inside string literals is not a parameter.
- **Unsafe**: calls (not `macro`/`fn` definitions) to `system(`, `signal(`, `override(`, `write_user(` → needs `--unsafe`
  (bpftrace refuses otherwise; dry-run stderr is the source of truth, the regex is a hint).
- The extractor must never panic on garbage input (fuzz-ish test with random bytes).

### 6.4 Validation (background)
Run on a bounded worker pool (default 4), results cached in memory keyed by
`(sha256(content), bpftrace_version, kernel_release)`.

Two strategies, chosen once at startup by capability detection (`bpftrace --help`):
1. **Dry-run (preferred)**: `bpftrace --dry-run -q -f json -- <file> [dummy params]`.
   Parses, loads **and attaches** probes then exits. Exit 0 → `●`. Non-zero → `✗` with stderr,
   or `!` when stderr is bpftrace refusing unsafe builtins (wording differs by version:
   "…need the --unsafe flag" / "…unsafe function being used in safe mode").
   Requires privileges. Positional params the script needs get placeholder values
   (`0`) so the dry run does not fail on missing args; note this in the Validation tab.
   Timeout 20 s per script → `✗ timeout`.
2. **Probe listing (fallback — old bpftrace or no privileges)**: for each extracted probe,
   `bpftrace -l '<probe>'`; ≥1 line → found. All found → `●`, some → `◐`, none → `✗`.
   Cache `-l` results per pattern. Mark the result as "heuristic" in the Info tab.

Kernel lockdown / Secure Boot: detect `/sys/kernel/security/lockdown` containing
`[integrity]` or `[confidentiality]` and show a persistent red banner; validation and
runs will fail and the banner explains why.

### 6.5 Execution
- Command: `[bpftrace] -f json -B line [--unsafe] -- <file> [positional…] [--named=val …]`.
  The script and every parameter go after `--`, so no user value can become a bpftrace
  option (D-013). Positional values starting with `--` are rejected (bpftrace would read
  them as named params).
- Spawn with `tokio::process::Command`, own process group (`process_group(0)`),
  `kill_on_drop(true)`, stdin null, stdout+stderr piped. No shell, ever.
- stdout: line reader → `json::parse_line` → `RunEvent` → mpsc → app state.
  Lines that are not valid JSON (some warnings leak to stdout) → log as raw.
- stderr: line reader → log (error style) and kept for the Validation tab.
- Backpressure: bounded channel (4096). If the UI lags, coalesce map/hist snapshots
  (only the latest per map name matters); never drop printf lines silently — count
  drops and show them in the header. Implemented in the executor's forwarder: events
  wait in a coalescer until the app channel has room, so batches are tiny while the UI
  keeps up; text lines beyond the log capacity (10k) are dropped oldest-first and
  counted; the exit event always comes after the exit-time dump.
- Keys and signals use their own channel, read before run output, and the loop applies
  queued messages for at most ~30 ms before drawing. Measured with the fake bpftrace
  flooding 10M printf lines: keys show on screen within 10–20 ms.

- Remote targets (§6.9): the same argv with the script path `script.bt`; the script is
  sent over the SSH session and deleted when the run ends. A stop writes a line to the
  session instead of signalling; the host side does SIGINT → SIGTERM (5 s) → SIGKILL (2 s)
  to bpftrace's process group, and a dropped connection stops it the same way.

### 6.6 Export (M6)
- `w` in the run view writes `<export dir>/bpfdeck-<script>-<UTC yyyymmdd-hhmmss>.txt`
  (header, panels in bpftrace's text format, the log) and `.ndjson` (bpftrace's raw stdout,
  byte for byte). `--export-dir` (default `.`) chooses the directory.
- The raw copy is spooled to `<cache>/bpfdeck/runs/` while the run is going (capped at
  256 MiB, noted in the text export when hit) and deleted when the next run starts or
  bpfdeck exits. Exporting works during a run too.

### 6.7 Readability under real-world noise
- Log: an error/raw line identical to one of the last 4 lines (with only error/raw lines
  in between) increments that line's `(×N)` counter instead of adding a line. Program
  output is never collapsed. The header's error count is still exact.
- Histogram panel: when the buckets don't fit, runs of 3+ empty buckets collapse into one
  `⋮ N empty buckets` row so far-out outliers stay visible. The text export keeps every
  bucket, like bpftrace.

### 6.8 Misc
- `e` opens the selected script in `$EDITOR` (suspend TUI, restore after). Read-only
  for git sources: copy to a temp file and warn that edits are not saved to the repo.
- `r` re-runs discovery + validation (e.g. after editing). It does not fetch git sources
  again (restart bpfdeck for that); unchanged scripts come back instantly from the
  validation cache. Closing `$EDITOR` on a local script triggers the same rescan.
- `$VISUAL`/`$EDITOR` (default `vi`) is split on whitespace and run without a shell.
- Inline editing (D-025): `i` turns the Source tab into an editor (full width, cursor,
  auto-indent, `Ctrl-Z` undo, `Esc` done). The result is a **draft** for this session:
  the source file is never written. While the draft is active, the list marks the script
  `✎`, and validation (on every target) and runs use the edited version; `u` switches
  between the original and the draft without losing either. Editing back to the original
  text drops the draft. bpftrace reads a private copy (`<cache>/drafts/<pid>/`, 0700/0600,
  removed on exit); a remote run sends the edited text. Drafts survive a rescan.
- Edits are always visible (D-026): the list shows `✎ +2 −1` (lines added/removed vs the
  original), the Info tab says so, the Source tab marks added lines `+` on green and
  shows removed lines crossed out on red with `-`; the run confirmation, the run header
  (`✎ edited +2 −1`) and its log say the run is of the edited version, and an export
  lists the changes at the end.
- Terminal min size 80×24; below that render a "terminal too small" message only.
- Mouse: not required in v1.

### 6.9 Remote targets (docs/design-remote.md, D-020…D-023)
- `c` (or `+`) opens the connect dialog: host (IP, name, `user@host`, `~/.ssh/config`
  alias), port, sudo mode (automatic: root login or `sudo -n`; root login; sudo with a
  password, typed masked and kept in memory while connected), optional bpftrace path.
- Checks, shown as they finish; the first hard failure stops with the reason: ssh
  (connected as, latency), host (OS, arch, kernel), shell tools (no `setsid` is a
  warning), root, bpftrace (version, path, `--dry-run`), kernel (lockdown, BTF).
  If SSH needs a person (passphrase, password, 2FA, unknown host key), `Enter` suspends
  the TUI and runs `ssh -fN` in the terminal; bpfdeck never sees those secrets.
- The system `ssh` is used (config, agent, ProxyJump, known_hosts apply), one master
  connection per target (`/tmp/bpfdeck-<uid>/<id>-%C`, 0700), every operation a session
  `ssh … -- host sh -s` over it, with a fixed runner program on stdin. Nothing is
  installed or left on the host.
- The source is resolved once, locally, and shared by all targets. On connect, every
  script is validated on the host (its own kernel and bpftrace).
- The master is checked every 10 s; when it is gone the tab turns `✗`, runs there end
  (the host stops bpftrace on EOF), `c` reconnects (same tab) and `d` closes the tab.
  `d` disconnects (asks while a run is active); quitting closes every master.
- Exports from a remote tab carry the host in the file name and the report.

## 7. Privileges

bpftrace needs root (or CAP_BPF + CAP_PERFMON + CAP_SYS_RESOURCE, plus CAP_SYS_ADMIN
on older kernels). v1 model: **run bpfdeck itself with sudo**. At startup:
- euid 0 → fine.
- not root → still start (browsing, reading, heuristic `-l` validation may partly work),
  status bar shows `NO PRIV` in red, run confirmation shows why it will fail.
Running as root is the model (D-018). On SSH targets the session becomes root through
the login (root), `sudo -n`, or `sudo -S` with the password from the connect dialog.

## 8. Non-functional

- Startup to first frame < 150 ms for a local dir of 200 scripts (validation is async).
- Idle CPU ~0 (no busy redraw; redraw on event or 250 ms tick while running).
- Render 60+ events/s from `printf`-heavy scripts without the UI stalling.
- Release binary < 10 MB, no runtime deps besides `bpftrace` and `git` (git only for URLs).
- Terminal always restored on panic, SIGTERM, SIGHUP; child bpftrace always killed.
- All colors from `src/ui/theme.rs` (Gruvbox dark). No hardcoded colors elsewhere.

## 9. Keymap (initial)

| Key | Context | Action |
|---|---|---|
| `j/k`, `↑/↓` | list | move |
| `g/G` | list | top/bottom |
| `t` | list | tree / flat list |
| `h/l`, `←/→`, `Enter` on a dir | tree | collapse (or go to parent) / expand / toggle |
| `/` | list, log | filter |
| `Enter` | list | run (confirm dialog) |
| `Tab`/`Shift-Tab`, `1-3` | detail | switch tab / panel |
| `PgUp/PgDn`, `Ctrl-u/d` | detail | scroll the detail pane |
| `e` | list | open in `$EDITOR` |
| `i` | list | edit inline (draft, `Esc` done, `Ctrl-Z` undo) |
| `u` | list | switch between the original and the edited version |
| `r` | list | rescan + revalidate |
| `x`, `Ctrl-C` | run view | stop run (SIGINT to bpftrace) |
| `j/k`, `PgUp/PgDn`, `g/G` | run view | scroll the log (up pauses, `G` follows) |
| `o` | list | show the last run |
| `c`, `+` | list, run view | connect to a host (new results tab) |
| `<` `>` | list, run view | previous/next target tab |
| `d` | list, run view | disconnect the selected host |
| `p` | log | pause/follow |
| `[` `]` | hist panel | previous/next key |
| `s` | table panel | toggle sort |
| `z` | run view | toggle full width |
| `Esc` | anywhere | back / close modal |
| `?` | anywhere | help |
| `q` | browser | quit (asks if a run is active) |
