# JSON fixtures

One bpftrace `-f json` message per line (NDJSON), exactly as bpftrace streams it.

Most files are compacted copies of bpftrace's own test expectations
(`tests/runtime/outputs/*.json` and `tests/runtime/json-output`, bpftrace/bpftrace,
Apache-2.0). `session_mixed.ndjson` is synthetic and models a realistic stream.

Before M2 is closed, capture real output on the target machines (RHEL 9, Debian 12/13)
with the scripts in `../scripts/` and add them here as `real_<distro>_<script>.ndjson`.
Field shapes may drift between bpftrace versions; the parser must tolerate that.
