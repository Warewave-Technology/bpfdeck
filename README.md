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

## Install
Releases ship static binaries (musl, no runtime dependencies) for x86_64 and aarch64:
```sh
sha256sum -c bpfdeck-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz.sha256
tar xzf bpfdeck-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz
sudo install bpfdeck-vX.Y.Z-x86_64-unknown-linux-musl/bpfdeck /usr/local/bin/
```

## Build
```sh
cargo build --release
# static binary, as in .github/workflows/release.yml:
rustup target add x86_64-unknown-linux-musl
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
  cargo build --release --target x86_64-unknown-linux-musl
```

## Remote hosts
Today: copy the static binary and run the TUI there over SSH (scripts must be on that host,
or use a git URL it can reach):
```sh
scp bpfdeck server01:/tmp/ && ssh -t server01 sudo /tmp/bpfdeck https://github.com/bpftrace/bpftrace
```
Driving a remote host from the local TUI is proposed in `docs/design-remote.md`.

## Keys worth knowing
`Enter` run (params form → confirmation) · `x` stop (SIGINT, keeps the exit-time dump) ·
`Tab` next panel · `w` export the run (`.txt` report + raw `.ndjson`, see `--export-dir`) ·
`?` all keys.

## Docs
- `docs/spec.md` — what it does
- `docs/architecture.md` — how it's built
- `docs/bpftrace-json.md` — bpftrace JSON output reference
- `docs/decisions.md` — decision log
