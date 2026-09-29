# Changelog

## 0.1.0 — first release

bpfdeck: a terminal UI for bpftrace script collections. Point it at a directory or a git
repository (`bpfdeck https://github.com/bpftrace/bpftrace`) and it lists the scripts,
tells you which ones run on this kernel, runs them and shows their output live.

### Browse and validate
- Discovers `.bt` files and bpftrace shebang scripts in a directory or git repository
  (`#branch`, `#tag` or `#commit` suffix; shallow fetch into a cache, no hooks, no
  submodules). Tree view for scripts in subdirectories, fuzzy filter.
- Reads each script's header: description, usage, probes, parameters (`$1…`,
  `getopt()`), unsafe calls.
- Validates every script in the background against the running kernel: `bpftrace
  --dry-run` where available, per-probe `bpftrace -l` otherwise; cached by content.
- Detects missing privileges and kernel lockdown and says why a script would fail.

### Run and visualize
- Parameters form, then a confirmation with the exact command line; `--unsafe` only
  when you turn it on for that run.
- Live panels from bpftrace's JSON output: histograms (log2 and linear, with far
  outliers kept visible), sorted top tables with change markers, values, stats, time
  series, and a filterable event log where repeated errors are counted, not repeated.
- Stopping sends SIGINT, so `END` runs and the final maps are shown; SIGTERM/SIGKILL
  follow on timeouts; bpftrace never outlives bpfdeck.
- `w` exports a run: a text report and the raw NDJSON.
- Inline editing (`i`): tweak a script before running it; the source file is never
  written, `u` switches between the original and your version, and every view (list,
  source, confirmation, run header, export) shows what was changed.

### Remote hosts and fleets
- `c` connects to hosts over your own `ssh` (config, agent, ProxyJump, known_hosts);
  nothing is installed on them. Root via login, `sudo -n` or sudo with a password.
  Several hosts at once: `db-0{1..4}`.
- Each host gets a results tab and its own validation of every script; lost
  connections are detected and can be reconnected.
- Fleet runs: run a script on several hosts at once and compare them side by side
  (histogram percentiles per host or merged, key × host tables, outlier marks), then
  export every host's run and a comparison report.

### Builds
- Static binaries (musl) for x86_64 and aarch64 Linux, under 5 MB, no runtime
  dependencies besides `bpftrace` (and `git`/`ssh` when used).
