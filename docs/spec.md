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
- Single host (the one bpfdeck runs on).

### Out of scope (v1) — do not build, do not stub
- BCC Python tools, libbpf/CO-RE binaries, raw BPF object files.
- Remote execution over SSH, fleet mode.
- Editing scripts inside the TUI (open in `$EDITOR` is fine, see §6.6).
- Persisting run history to disk (M6 is optional export only).
- Any network access other than the `git` subprocess for cloning.

## 4. Core flow

```
bpfdeck <path|git-url>
  → resolve source (clone/update into cache if git)
  → discover scripts (walk tree)
  → parse metadata (header comment, probes, params, unsafe calls)
  → validate against this kernel (background, cached per script hash)
  → user browses list, reads source/info
  → user runs a script: confirm dialog → params form (if any) → run view
  → live output: hist / map table / stats / log
  → stop (SIGINT → final END output → exit) → output stays viewable
```

## 5. Screens

### 5.1 Browser (default screen)
Two panes + status bar.

Left: script list, grouped by directory (collapsible tree or flat list with a dim path
prefix — pick flat first, tree later). Each row:

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
| `▶` | Currently running | `Theme::running` |

Right pane, tabs (`Tab`/`Shift-Tab` or `1..3`):
1. **Info** — description, usage lines from header, probes list with per-probe ✓/✗,
   parameters detected, flags (unsafe, uses `-c`/`-p` semantics hint), file path, size.
2. **Source** — the script with line numbers and light syntax highlighting (comments,
   strings, probe lines, builtins, `@maps`). Hand-written highlighter, no tree-sitter in v1.
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

### 5.3 Parameters form (modal, only if the script uses parameters)
- Positional: every `$1..$N` used in the script → one text field each. `$#` usage noted.
- Named: every `getopt("name")` / `getopt("name", default)` → field prefilled with the default;
  boolean when called with one argument or a bool default → checkbox.
- USAGE lines from the header are shown above the form as help.
- Values are passed as separate argv entries (never through a shell).

### 5.4 Run view
Replaces the right pane (list stays visible, can be hidden with `z` for full width).

Header line: script name, elapsed time, attached probe count (from `attached_probes`),
state (`starting | running | stopping | exited(code) | failed`).

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

Map semantics: each new message for a map name **replaces** the previous snapshot
(bpftrace prints the whole map each time, and scripts usually `clear()` after print).
Keep the previous snapshot to show deltas (↑/↓ markers) in the top table.

Stop: `Ctrl-C` or `x` inside run view sends SIGINT to bpftrace (NOT to bpfdeck), which
makes bpftrace run `END` and print remaining maps — those final messages must be
captured and rendered. After 5 s without exit → SIGTERM, after 2 s more → SIGKILL.
Leaving the run view with `Esc` does not stop the run; the list row keeps `▶`.

v1: one run at a time. Starting another asks to stop the current one.

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
   Parses, loads **and attaches** probes then exits. Exit 0 → `●`. Non-zero → `✗` with stderr.
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
  drops and show them in the header.

### 6.6 Misc
- `e` opens the selected script in `$EDITOR` (suspend TUI, restore after). Read-only
  for git sources: copy to a temp file and warn that edits are not saved to the repo.
- `r` re-runs discovery + validation (e.g. after editing).
- Terminal min size 80×24; below that render a "terminal too small" message only.
- Mouse: not required in v1.

## 7. Privileges

bpftrace needs root (or CAP_BPF + CAP_PERFMON + CAP_SYS_RESOURCE, plus CAP_SYS_ADMIN
on older kernels). v1 model: **run bpfdeck itself with sudo**. At startup:
- euid 0 → fine.
- not root → still start (browsing, reading, heuristic `-l` validation may partly work),
  status bar shows `NO PRIV` in red, run confirmation shows why it will fail.
See decisions D-005 for the planned privilege-separated model.

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
| `/` | list, log | filter |
| `Enter` | list | run (confirm dialog) |
| `Tab`/`Shift-Tab`, `1-3` | detail | switch tab / panel |
| `e` | list | open in `$EDITOR` |
| `r` | list | rescan + revalidate |
| `x`, `Ctrl-C` | run view | stop run (SIGINT to bpftrace) |
| `p` | log | pause/follow |
| `[` `]` | hist panel | previous/next key |
| `s` | table panel | toggle sort |
| `z` | run view | toggle full width |
| `Esc` | anywhere | back / close modal |
| `?` | anywhere | help |
| `q` | browser | quit (asks if a run is active) |
