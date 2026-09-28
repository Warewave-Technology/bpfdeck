# Real-kernel checks without a Linux box (D-016)

`tests/realhost/run.sh` builds bpfdeck and runs it in a privileged Debian 13 container
(bpftrace 0.23.2) on the Docker VM's kernel — OrbStack on the owner's Mac (kernel 7.0,
aarch64, BTF present, no lockdown). tracefs is mounted inside the container.

```sh
tests/realhost/run.sh --list tests/fixtures/scripts   # real --dry-run validation
tests/realhost/run.sh tests/fixtures/scripts          # TUI against real bpftrace
```

It is a stand-in, not a replacement for RHEL 8/9 and Debian 12 hosts (D-008): one
bpftrace version, one unusual kernel, pid namespaces. Known quirks of this environment:

- `kprobe:vfs_read` / `fentry:vfs_read` never fire (so `vfs_latency_demo.bt` shows no
  histogram); syscall tracepoints do. A read-latency script on `tracepoint:syscalls:*`
  was used instead for the histogram and exit-dump checks.
- `tid`/`pid` in a container pid namespace make `get_ns_current_pid_tgid` fail: tens of
  thousands of `helper_error` messages per second. Useful as a stress case.

## Findings (2026-09-28)

| Check | Result |
|---|---|
| `--list` verdicts | ● for runnable scripts, ✗ missing kprobe, ! unsafe, ✗ `getopt` (absent in 0.23) |
| Unsafe refusal wording | 0.23: "…is an unsafe function being used in safe mode" (no `--unsafe` in the text) → fixed in `validate::needs_unsafe` |
| stdout non-JSON lines | Only blank lines (2 per run, before the exit dump), also with `-q` |
| `attached_probes` | `data.probes` only, no top-level `count`; absent with `-q` |
| Exit-time dump | After SIGINT: blank lines, then the maps; `count()` maps print even at 0 |
| helper errors | Very frequent in real scripts → the log now counts repeats (`(×N)`) |
| Histogram outliers | Real data had 3 samples 32 empty buckets away → the panel collapses empty runs |
| TUI run + stop + export | Worked end to end; export of a 150k-error run: 19 MB NDJSON, 645 KB text |

Fixtures from these runs: `tests/fixtures/json/real_debian13_orbstack_*.ndjson`
(process names other than the test's own are anonymized to `proc-N`).
