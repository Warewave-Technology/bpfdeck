# Decisions

Format: ID — decision — why — status. Add new entries at the bottom; never rewrite old
ones, supersede them.

**D-001 — bpftrace only in v1.** BCC/libbpf tools have unstructured output or no
common discovery convention; bpftrace gives `-f json`, `--dry-run` and `-l`. — accepted

**D-002 — Rust + ratatui 0.30, tokio.** Owner's choice; single static-ish binary fits
servers. — accepted

**D-003 — Theme: Gruvbox dark only, via `src/ui/theme.rs` semantic roles.** No theme
switching in v1, but no hardcoded colors outside `theme.rs` so it stays possible. — accepted

**D-004 — Shell out to `git`, don't link libgit2/gix.** Avoids OpenSSL/libgit2 build
pain on RHEL, respects user's SSH/credential config, smaller binary. Hooks disabled,
`--depth 1`, no submodules. — accepted

**D-005 — Privileges: v1 requires running bpfdeck as root.** Simple and honest. Planned
v2: bpfdeck unprivileged, spawns `sudo -n bpftrace …` (or a tiny setcap helper) so the
TUI, git clone and script parsing never run as root. — accepted for v1, revisit after M5

**D-006 — Validation prefers `--dry-run`, falls back to `-l` per probe.** `--dry-run`
parses, loads and attaches, so it is authoritative; `-l` is a heuristic for old
bpftrace or no privileges. Capability detected from `bpftrace --help`. — accepted

**D-007 — Parse JSON via `serde_json::Value` + manual dispatch on `type`.** Output shapes
drift between bpftrace versions; strict typed enums would break. Unknown types are
logged raw, never an error. — accepted

**D-008 — Target platforms: RHEL 8/9 family and Debian/Ubuntu, x86_64 first,
aarch64 nice-to-have.** Same fleet profile as hofund. Old packaged bpftrace on RHEL 8 is
the reason for D-006's fallback. — accepted

**D-009 — `--unsafe` is never passed implicitly.** Only after an explicit toggle in the
run confirmation dialog, per run. Scripts from a git repo are untrusted code running in
the kernel as root. — accepted

**D-010 — One run at a time in v1.** Multiple concurrent runs complicate layout and
signal handling; the state model (`RunId`) is designed so it can be added later. — accepted

**D-011 — Working name `bpfdeck`.** Placeholder; rename before first release. — open

**D-012 — Git sources: `init` + shallow `fetch` + `reset --hard` instead of `clone`.**
Refines §6.1 of the spec. First checkout and update are the same code path
(`fetch --depth 1 -- origin <ref|HEAD>` → `reset --hard FETCH_HEAD`), which also covers
tags and full commit hashes that `clone --branch` cannot take; abbreviated hashes fall back
to a full fetch and a local `rev-parse`. Every git call forces `core.hooksPath=/dev/null`,
`submodule.recurse=false`, `core.fsmonitor=false`, `protocol.ext.allow=never` and
`GIT_TERMINAL_PROMPT=0`. `#ref` is user input that lands in git's argv, so refs starting
with `-` or containing whitespace/control characters are rejected and refs are placed
after `--`. Recognized URL prefixes: `https://`, `http://`, `ssh://`, `git://`, `file://`,
`git@` (`file://` makes the git path testable without network). A failed first checkout
removes the cache dir; a failed update keeps the old checkout and returns a warning. — accepted

**D-013 — Script and all parameters go after `--`.** Refines spec §6.5. bpftrace parses
options with GNU `getopt_long`, which permutes: an option-looking argument is an option
anywhere on the command line unless it follows `--` (verified in `src/main.cpp`: after
getopt the first remaining argument is the script, `--x[=v]` are named params, the rest
positional). With the spec's `<file> <positional…> -- <named…>`, a positional value such
as `--unsafe` or `-o /etc/passwd` would become a bpftrace option while running as root.
So the argv is `bpftrace -f json -B line [--unsafe] -- <file> [positional…] [--name=v…]`
(same for `--dry-run` and `-l -- <probe>`). bpftrace itself cannot tell a positional value
starting with `--` from a named param, so the builder rejects such values. — accepted

**D-014 — Parameters form before the run confirmation.** Resolves a contradiction in the
spec (§4 said confirm → params, §5.2 wants the confirmation to show the exact command
line *after* params). The form comes first; the confirmation then shows the final argv
and is the single last gate (D-009's `--unsafe` toggle lives there). Named params left at
their default are not passed, so the command line shows only what the user changed; a
flag whose default is `true` that the user unticks is passed as `--name=false`. — accepted

**D-015 — Releases: static musl binaries linked with rust-lld, built on one x86_64 runner.**
A `v*` tag builds `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` with the
toolchain's bundled `rust-lld` as linker (musl targets carry their own C runtime), checks
the result is statically linked and < 10 MB, runs the x86_64 one, and attaches
`bpfdeck-<tag>-<target>.tar.gz` + `.sha256` to a GitHub release. No `cross`, Docker or C
cross toolchain. Verified locally: both targets built (aarch64 also cross-built from
amd64) and ran on Alpine (no glibc); 3.3 MB / 3.9 MB. CI builds the x86_64 musl binary on
every push. Nothing is pushed or released until the owner decides to. — accepted

**D-016 — Real-kernel checks in a privileged container on the Docker VM kernel.**
Without a Linux host at hand, `tests/realhost/` runs bpfdeck against bpftrace 0.23.2 on
OrbStack's kernel. It found a real bug (0.23's unsafe wording) and two UX problems
(helper-error floods, far outliers in histograms) that the fake could not. It does not
replace the RHEL/Debian checks of D-008; see docs/real-kernel-testing.md. — accepted

**D-017 — The name stays `bpfdeck`.** Supersedes D-011 (owner's call, 2026-09-29): crate,
binary, cache dir and docs keep the name. — accepted

**D-018 — Running as root is the model, not a stopgap.** Supersedes the "revisit after M5"
part of D-005: bpfdeck is an admin tool; privilege separation (unprivileged TUI, `sudo -n`
or setcap helper) is not planned. Remote execution (docs/design-remote.md) follows the
same model on the remote side. — accepted

**D-019 — Tree view is the default for scripts in subdirectories.** The flat list stays
for flat collections and for filter results (fuzzy ranking does not map onto a tree).
Rows are derived from the filtered script list on each change (`app/tree.rs`, pure), so
the filter, validation and run state need no tree awareness. — accepted

**D-020 — Remote execution installs nothing on the target.** Owner's call (2026-09-29): an
agent binary on hosts is not acceptable; any server reachable over SSH with admin rights
must work as is, since the point is fixing problems on machines nobody prepared. The
agent proposal (docs/design-remote.md history, c466bcc) is withdrawn; the agentless design
(a fixed POSIX sh runner sent over `ssh host sh -s`) is in docs/design-remote.md, awaiting
approval. — accepted

**D-021 — Targets as result tabs; one run per target.** Supersedes D-010's single run:
the owner approved docs/design-remote.md (2026-09-29) with the connect dialog inside the
TUI (`c`/`+`, no `--host` flag), `<` `>` to switch tabs, `d` to disconnect, the `local`
tab always present (red `no bpftrace` when missing) and sudo with a password. Each target
has its own validation results and at most one run; runs on different targets can go
on together. The source is resolved once, locally, for all targets. — accepted

**D-022 — Remote implementation details.** (1) One ssh master per *target*, socket
`/tmp/bpfdeck-<uid>/<id>-%C` (0700, owner-checked, short for the ~104-byte limit): with a
per-host `%C` a retry met the previous attempt's socket and left a non-master `ssh -N`
running, and two tabs for one `user@host:port` would share and close one master. A
failed connect attempt closes its master. (2) Masters use ServerAlive keepalives and are
checked with `ssh -O check` every 10 s; a dead one marks the tab lost. (3) Host facts come
from a fixed sh program sent as the session's script (`sh script.bt`), because argv lines
cannot carry a multi-line program. (4) The sudo password lives only in the executor's
`SshTarget` (its `Debug` is redacted), is sent over the session's stdin after the
`bpfdeck-remote: sudo` marker for each operation, and is never on a command line. (5) A
remote run's handle exists before its handshake ends (`runner::start_deferred`), so a
stop or quit during the handshake is not lost. — accepted

**D-023 — SIGINT and the runner's background job.** POSIX sh starts background jobs with
SIGINT ignored, and a shell script cannot trap a signal ignored on entry. The runner
un-ignores it (`trap - INT QUIT` in the job's subshell) where the shell allows it (bash 5,
zsh; not dash or bash 3.2). Real bpftrace installs its own handler, so on hosts this only
matters for shell wrappers around bpftrace; for tests, the fake host's `sh`
(`tests/fake_ssh/bin/sh`) prefers zsh or bash 5 so the fake bpftrace sees the SIGINT and
prints its exit-time dump. — accepted

**D-024 — Fleet mode as proposed.** The owner approved docs/design-fleet.md unchanged
(2026-09-29): 2–20 targets; the target checklist in the run confirmation (no new key);
the compare tab (per-host percentiles, merged hist with `m`, key × host tables, `◀` at 3×
the median); a host list with `{a..b}` ranges in the connect dialog with one sudo mode for
all; hosts whose validation failed unchecked by default but selectable. — accepted

**D-025 — Inline editing makes session drafts, never writes the source.** Owner's request
(2026-09-29), supersedes the "no editing inside the TUI" line of spec §3. The typical case
is tweaking a script from a git source (a cache, not the user's files) before running it
on a host. So `i` edits in the Source tab and `Esc` keeps the result as a draft; the
original stays as scanned, `u` flips between the two, and editing back to the original
drops the draft. bpftrace reads the draft from a private copy the executor writes
(the app does no I/O); the copy is validated and run like any script, cached by its
content hash, and removed on exit. A small hand-written buffer (`model/editor.rs`) instead
of a new crate: no dependency to approve, and scripts are small. — accepted

**D-026 — Inline edits are always visible, down to the line.** Owner's request
(2026-09-29): a draft must never be mistaken for the original. A small LCS line diff
(`model/diff.rs`, pure) against the original drives every place a script or run shows
up: `✎ +a −r` in the list, the Info tab, `+`/`-` lines in the Source tab, the run
confirmation, the run header and log, and a "changes vs the source file" section in
exports, so a report says exactly what ran. — accepted

**D-027 — One log line per distinct error; no failure text in the list.** Owner's
request (2026-09-29) after a real run: helper errors that alternate with program output
(e.g. `pid` in a container's pid namespace) were not collapsed by the "last 4 lines" rule
and filled half of the log. Now every repeat of an error text, anywhere in the run, counts
on its first line, `(×N, last mm:ss)`; the order of individual errors is in the raw NDJSON
export. Raw lines keep the in-a-row rule (unknown output is data). In the script list,
a failed validation shows only `✗`: the reason pushed the description off the row and is
in the Info and Validation tabs. — accepted
