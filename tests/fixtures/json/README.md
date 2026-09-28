# JSON fixtures

One bpftrace `-f json` message per line (NDJSON), exactly as bpftrace streams it.

Most files are compacted copies of bpftrace's own test expectations
(`tests/runtime/outputs/*.json` and `tests/runtime/json-output`, bpftrace/bpftrace,
Apache-2.0). `session_mixed.ndjson` is synthetic and models a realistic stream.

Before M2 is closed, capture real output on the target machines (RHEL 9, Debian 12/13)
with the scripts in `../scripts/` and add them here as `real_<distro>_<script>.ndjson`.

`real_debian13_orbstack_*.ndjson`: bpftrace 0.23.2 (Debian 13) on OrbStack's kernel, see
docs/real-kernel-testing.md. Kept verbatim (blank lines included) except: process names
other than the test's own (`cat`, `true`, `sleep`, `bash`, `bpftrace`) are anonymized to
`proc-N`, and `readlat` keeps only the first 3 of ~2k repeated helper errors.
Field shapes may drift between bpftrace versions; the parser must tolerate that.
