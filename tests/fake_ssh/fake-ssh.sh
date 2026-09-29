#!/bin/sh
# Fake ssh for tests: ignores the host and runs the remote command locally, with
# tests/fake_ssh/bin (fake sudo, id and sh) and bpftrace-bin (the fake bpftrace as
# `bpftrace`) first in PATH. Behavior by host name:
#   unreachable*  → "Connection refused", exit 255
#   needs-auth*   → a BatchMode master fails with "Permission denied" (interactive works)
#   pwsudo*       → the fake sudo requires a password ("secret")
#   rootlogin*    → logged in as root (no sudo needed)
#   nobpftrace*   → no bpftrace in PATH
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
[ -n "$control" ] && exit 0
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
  *rootlogin*) FAKE_ROOT=1; export FAKE_ROOT ;;
esac
here="$(cd "$(dirname "$0")" && pwd)"
case "$host" in
  *nobpftrace*) PATH="$here/bin:$PATH" ;;
  *) PATH="$here/bin:$here/bpftrace-bin:$PATH" ;;
esac
export PATH
exec "$@"
