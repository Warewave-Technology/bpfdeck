#!/bin/sh
# Build and run bpfdeck against a real kernel in a privileged Debian 13 container.
#   tests/realhost/run.sh tests/fixtures/scripts           # TUI
#   tests/realhost/run.sh --list tests/fixtures/scripts    # real --dry-run validation
# The container loads BPF programs into the Docker VM's kernel only.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
docker build -q -t bpfdeck-realhost "$root/tests/realhost" >/dev/null
tty=""
if [ -t 0 ] && [ -t 1 ]; then tty="-t"; fi
exec docker run --rm -i $tty --privileged \
  -v "$root":/src:ro -v bpfdeck-cargo:/usr/local/cargo/registry -v bpfdeck-target:/target \
  -e CARGO_TARGET_DIR=/target -e TERM="${TERM:-xterm-256color}" -w /src bpfdeck-realhost \
  sh -c 'mountpoint -q /sys/kernel/tracing || mount -t tracefs nodev /sys/kernel/tracing
         cargo build -q && exec /target/debug/bpfdeck "$@"' bpfdeck "$@"
