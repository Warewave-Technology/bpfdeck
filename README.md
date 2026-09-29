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
Press `c` in the TUI, type a host (`10.0.3.14`, `ops@db-02`, or an `~/.ssh/config` alias;
several at once: `db-01 db-02` or `db-0{1..4}`),
pick how to become root (automatic: root login or `sudo -n`; root login; sudo with a
password), `Enter`. bpfdeck checks SSH, root, bpftrace and the kernel, opens a results tab
for the host and validates every script there. From then on `Enter` runs the script on
the selected tab's host: the script is copied over the SSH session for that run and
removed afterwards. `<` `>` switch tabs, `d` disconnects.

Nothing is installed on the host: it needs `bpftrace`, a POSIX `sh` and coreutils. Your
`ssh` and its config are used as they are (keys, agent, ProxyJump, known_hosts); if SSH
has to ask for a passphrase or password, bpfdeck hands it the terminal. Details:
`docs/design-remote.md`. To try it against a container: `tests/realhost/sshd.sh`.

## Keys worth knowing
`Enter` run (params form → confirmation) · `x` stop (SIGINT, keeps the exit-time dump) ·
`c` connect a host · `<` `>` target tabs · `d` disconnect · `i` edit a script inline
(a draft: the file is never changed; `u` switches back to the original) ·
`Tab` next panel · `w` export the run (`.txt` report + raw `.ndjson`, see `--export-dir`) ·
`?` all keys.

## Docs
- `docs/spec.md` — what it does
- `docs/architecture.md` — how it's built
- `docs/bpftrace-json.md` — bpftrace JSON output reference
- `docs/decisions.md` — decision log
