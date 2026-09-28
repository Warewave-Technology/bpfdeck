# bpfdeck

A terminal UI for bpftrace script collections.

```sh
sudo bpfdeck https://github.com/bpftrace/bpftrace     # or a local directory
```

- Lists every bpftrace script in a directory or git repo
- Tells you which ones will actually run on this kernel (`--dry-run` / probe checks)
- Runs them with a confirmation step and a form for script parameters
- Shows output live: histograms, sorted top-tables, stats, and a filterable event log
- Gruvbox dark

Status: early development — see `docs/milestones.md`.

## Requirements
- Linux, `bpftrace` in `PATH` (or `--bpftrace <path>`), root for running scripts
- `git` for git sources

## Build
```sh
cargo build --release
```

## Docs
- `docs/spec.md` — what it does
- `docs/architecture.md` — how it's built
- `docs/bpftrace-json.md` — bpftrace JSON output reference
- `docs/decisions.md` — decision log
