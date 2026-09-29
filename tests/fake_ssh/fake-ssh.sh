#!/bin/sh
# Fake ssh for tests: ignores the host and runs the remote command locally, with
# tests/fake_ssh/bin (a fake sudo) first in PATH. Behavior by host name:
#   unreachable*  → "Connection refused", exit 255
#   needs-auth*   → "Permission denied", exit 255 (interactive auth needed)
#   pwsudo*       → the fake sudo requires a password ("secret")
# `-O check|exit` and master connections (`-f -N`) just succeed.
control=""
master=0
while [ $# -gt 0 ]; do
  case "$1" in
    --) shift; break ;;
    -o|-p|-l|-F|-S) shift 2 ;;
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
  *needs-auth*) echo "$host: Permission denied (publickey,password)." >&2; exit 255 ;;
esac
[ -n "$control" ] && exit 0
[ "$master" = 1 ] && exit 0
case "$host" in
  *pwsudo*) FAKE_SUDO_PASSWORD=1; export FAKE_SUDO_PASSWORD ;;
esac
PATH="$(cd "$(dirname "$0")" && pwd)/bin:$PATH"
export PATH
exec "$@"
