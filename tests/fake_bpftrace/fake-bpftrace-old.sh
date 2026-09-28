#!/bin/sh
# An old bpftrace (v0.9.4, e.g. RHEL 8 era) without --dry-run. See fake-bpftrace.sh.
FAKE_BPFTRACE_OLD=1 exec "$(dirname "$0")/fake-bpftrace.sh" "$@"
