#!/bin/sh
# Fake bpftrace for tests (docs/architecture.md, "Testing without a kernel").
# POSIX sh; runs on Linux (dash/bash) and macOS.
#
#   --version / --help        v0.99.0 with --dry-run; set FAKE_BPFTRACE_OLD=1 (see
#                             fake-bpftrace-old.sh) for an old version without it.
#   -l -- <probe>             prints the probe, or nothing if it contains "does_not_exist".
#   --dry-run ... -- <file>   fails like bpftrace for missing probes and for unsafe calls
#                             without --unsafe; otherwise exit 0.
#   ... -- <file> [args]      run mode: replays an NDJSON fixture, then waits for SIGINT,
#                             prints a final hist dump (like END/exit-time map printing)
#                             and exits 0.
#
# Behaviour is driven by directives inside the script file, one per line:
#   // fake: replay=<fixture in tests/fixtures/json>   // fake: delay=<seconds, default 0.01>
#   // fake: stderr=<line>        // fake: exit=<code>   (exit right after replay)
#   // fake: ignore_int=1         // fake: ignore_term=1
#   // fake: child=1              (leave a background sleep in the process group)
#   // fake: dryrun_sleep=<secs>  // fake: argv_log=<path>   (append argv, one arg per line)

here=$(cd "$(dirname "$0")" && pwd)

if [ "${FAKE_BPFTRACE_OLD:-}" = 1 ]; then
  version="v0.9.4"
else
  version="v0.99.0-fake"
fi

case "${1:-}" in
  --version|-V)
    echo "bpftrace $version"
    exit 0
    ;;
  --help|-h)
    if [ "${FAKE_BPFTRACE_OLD:-}" = 1 ]; then
      echo "USAGE:" >&2
      echo "    bpftrace [options] filename" >&2
      echo "    -l [search]    list probes" >&2
      exit 1
    fi
    echo "USAGE:"
    echo "    bpftrace [options] filename"
    echo "    -l [search|filename]"
    echo "TROUBLESHOOTING OPTIONS:"
    echo "    --dry-run      terminate execution right after attaching all the probes"
    exit 0
    ;;
  -l)
    shift
    [ "${1:-}" = "--" ] && shift
    case "${1:-}" in
      *does_not_exist*) ;;
      *) echo "$1" ;;
    esac
    exit 0
    ;;
esac

dry_run=0
unsafe=0
while [ $# -gt 0 ]; do
  case "$1" in
    --) shift; break ;;
    --dry-run) dry_run=1 ;;
    --unsafe) unsafe=1 ;;
    -q) ;;
    -f|-B) shift ;;
    *) echo "ERROR: fake bpftrace: unexpected option $1" >&2; exit 64 ;;
  esac
  shift
done
script="${1:-}"
if [ -z "$script" ] || [ ! -f "$script" ]; then
  echo "ERROR: Could not read file: $script" >&2
  exit 1
fi

directive() {
  sed -n "s|^// fake: $1=||p" "$script" | head -n 1
}

log=$(directive argv_log)
if [ -n "$log" ]; then
  for arg in "$0" "$@"; do printf '%s\n' "$arg"; done >> "$log"
  echo "--end--" >> "$log"
fi

if [ "$dry_run" = 1 ]; then
  if [ "$version" = "v0.9.4" ]; then
    echo "bpftrace: unrecognized option '--dry-run'" >&2
    exit 1
  fi
  secs=$(directive dryrun_sleep)
  [ -n "$secs" ] && sleep "$secs"
  if grep -q 'does_not_exist' "$script"; then
    probe=$(grep -o 'kprobe:[A-Za-z0-9_]*does_not_exist[A-Za-z0-9_]*' "$script" | head -n 1)
    echo "stdin:1:1-30: ERROR: $probe: No such file or directory" >&2
    exit 1
  fi
  if [ "$unsafe" = 0 ] && grep -q 'system(' "$script"; then
    echo "stdin:7:3-17: ERROR: system() is unsafe. To use you need the --unsafe flag" >&2
    exit 1
  fi
  exit 0
fi

# ---- run mode ----
[ "$(directive ignore_int)" = 1 ] && trap '' INT
[ "$(directive ignore_term)" = 1 ] && trap '' TERM
if [ "$(directive ignore_int)" != 1 ]; then
  trap 'echo "{\"type\": \"hist\", \"data\": {\"@final\": [{\"min\": 0, \"max\": 0, \"count\": 1}]}}"; exit 0' INT
fi

[ "$(directive child)" = 1 ] && sleep 30 &

err=$(directive stderr)
[ -n "$err" ] && echo "$err" >&2

replay=$(directive replay)
delay=$(directive delay)
delay=${delay:-0.01}
if [ -n "$replay" ]; then
  while IFS= read -r line; do
    printf '%s\n' "$line"
    sleep "$delay"
  done < "$here/../fixtures/json/$replay"
fi

code=$(directive exit)
[ -n "$code" ] && exit "$code"

while :; do
  sleep 0.05
done
