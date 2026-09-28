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
