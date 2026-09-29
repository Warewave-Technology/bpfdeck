#!/bin/sh
# Fake ssh for tests: ignores the host and runs the remote command locally, with
# tests/fake_ssh/bin (fake sudo, id and sh) and bpftrace-bin (the fake bpftrace as
# `bpftrace`) first in PATH. Behavior by host name:
#   unreachable*  → "Connection refused", exit 255
#   needs-auth*   → a BatchMode master fails with "Permission denied" (interactive works)
#   pwsudo*       → the fake sudo requires a password ("secret")
#   rootlogin*    → logged in as root (no sudo needed)
#   nobpftrace*   → no bpftrace in PATH
#   dropped*      → `-O check` fails (the master connection is gone)
# `-E file` sends ssh's own messages there. `-O check|exit` and masters (`-f -N`) succeed.
control=""
master=0
batch=0
while [ $# -gt 0 ]; do
  case "$1" in
    --) shift; break ;;
    -o) [ "$2" = BatchMode=yes ] && batch=1; shift 2 ;;
    -E) exec 2>> "$2"; shift 2 ;;
    -p|-l|-F|-S) shift 2 ;;
    -O) control=$2; shift 2 ;;
    -f|-N|-fN) master=1; shift ;;
    -*) shift ;;
    *) break ;;
  esac
done
host=$1
shift
case "$host" in
  *unreachable*) echo "ssh: connect to host $host port 22: Connection refused" >&2; exit 255 ;;
esac
if [ -n "$control" ]; then
  case "$host:$control" in
    *dropped*:check) echo "Control socket connect: No such file or directory" >&2; exit 255 ;;
  esac
  exit 0
fi
if [ "$master" = 1 ]; then
  case "$host" in
    *needs-auth*) [ "$batch" = 1 ] && { echo "$host: Permission denied (publickey,password)." >&2; exit 255; } ;;
  esac
  exit 0
fi
case "$host" in
  *pwsudo*) FAKE_SUDO_PASSWORD=1; export FAKE_SUDO_PASSWORD ;;
esac
case "$host" in
  *rootlogin*) FAKE_ROOT=1 ;;
  *) FAKE_ROOT=0 ;;
esac
export FAKE_ROOT
here="$(cd "$(dirname "$0")" && pwd)"
case "$host" in
  *nobpftrace*)
    # Only the tools the runner and the facts program use, so a bpftrace installed on
    # this machine (CI runners have one in /usr/bin) is not found either.
    tools="${TMPDIR:-/tmp}/bpfdeck-fake-ssh-tools-$(/usr/bin/id -u)"
    mkdir -p "$tools"
    for t in bash dash zsh mktemp head rm setsid sleep cat sed uname grep tr cut env dirname; do
      p=$(command -v "$t" 2> /dev/null) && [ -n "$p" ] && ln -sf "$p" "$tools/$t"
    done
    PATH="$here/bin:$tools"
    ;;
  *) PATH="$here/bin:$here/bpftrace-bin:$PATH" ;;
esac
export PATH
exec "$@"
