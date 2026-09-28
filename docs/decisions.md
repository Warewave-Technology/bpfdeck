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
