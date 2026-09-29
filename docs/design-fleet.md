# Fleet mode: one script on several targets — design proposal

**Status: approved as proposed (D-024, 2026-09-29). F1–F4 done.** In F4 the `◀` rule for
histograms looks at p50 as well as p99 (a host that is slow for most requests stands out
even when every host has the same tail); the column that crosses the line is yellow. F3 adds `X` in a host
tab to stop the whole fleet run (the compare tab of F4 will also stop it with `x`). In F2 the list
count follows the glyph's name (`syscount 2/3`) instead of sitting between glyph and name,
so names stay aligned. One change in F1: a failed
row shows its failing check inline instead of opening with `o` (letters are typed into
the host field, and the failing check is the part that matters).
Builds on the remote targets (docs/design-remote.md, D-020…D-023). Questions are at the end.

## Goal

An SRE sees a symptom on "some of the db nodes". Today bpfdeck can connect to each node
and run the same script tab by tab. Fleet mode makes that one action and adds the thing
tabs cannot give: **the hosts side by side**, so the odd one out is visible at a glance
(the node whose p99 disk latency is 10× the others, the one where `postgres` makes most
of the syscalls).

- Connect several hosts in one go.
- Validate on all of them and show the result as a matrix (script × host).
- Run the selected script on the chosen targets at once, with one parameters form and
  one confirmation; each host's run goes to its own tab, as today.
- A **compare** tab shows every map of that run across hosts.
- Stop, export and quit work on the whole fleet run as well as per host.

Scale: **2–20 targets**. That covers "the nodes of one service" and keeps tabs, columns
and SSH sessions manageable. Hundreds of hosts need a different UI (aggregates only, no
per-host tabs) and are out of scope.

## User experience

### Connecting several hosts

The Host field of the connect dialog takes a list: `db-01 db-02 db-03`, with commas, or
with a brace range, `db-0{1..3}` / `10.0.3.{11..14}` (expanded locally, no shell). One
port, sudo mode and password apply to all of them. The checks run in parallel and turn
into one row per host:

```
╭ Connect to hosts ─────────────────────────────────────────────────────────────────╮
│  Host      db-0{1..4}█                  4 hosts                                  │
│  sudo      (•) automatic   ( ) root login   ( ) sudo with password                │
│                                                                                   │
│  ✓ db-01   Rocky 9.4 · 5.14.0-427 · bpftrace v0.21.2 · root via sudo     120 ms   │
│  ✓ db-02   Rocky 9.4 · 5.14.0-427 · bpftrace v0.21.2 · root via sudo      98 ms   │
│  ✗ db-03   root: sudo needs a password here                                       │
│  … db-04   bpftrace                                                               │
│                                                                                   │
│  Enter connect the rest · o open failures · Esc close (connected hosts stay)      │
╰───────────────────────────────────────────────────────────────────────────────────╯
```

- Every host that passes gets its tab; failures stay in the list with their reason, and
  `o` shows the full check list of the selected row (the single-host view of today).
- If some hosts need interactive SSH auth, they are done one after the other in the
  terminal (the existing handoff), then the checks continue.
- A single host behaves exactly as today.

### Validation matrix

The Validation tab of a script gets one line per target instead of only the active one:

```
Result on each target
  local        · no bpftrace
  db-01        ● runs (dry-run)
  db-02        ● runs (dry-run)
  db-04        ✗ ERROR: kprobe:blk_account_io_done: No such file or directory
```

The list glyph stays the active target's; with more than one usable target a small
count is added when hosts disagree (`● 3/4`), so a script that works only somewhere
stands out while scrolling.

### Running on several targets

`Enter` works as today. When more than one target can run scripts, the confirmation
gets a target checklist; the active target is checked, the others are not:

```
╭ Run biolatency.bt ────────────────────────────────────────────────────────────────╮
│ Command   bpftrace -f json -B line -- script.bt                                  │
│ Targets   [x] db-01  ● root via sudo                                              │
│           [x] db-02  ● root via sudo                                              │
│           [ ] db-04  ✗ validation failed: kprobe:blk_account_io_done not found    │
│           [ ] local  no bpftrace (cannot be checked)                              │
│ Enter run on 2 targets · Space toggle · a all that validated · Esc cancel         │
╰───────────────────────────────────────────────────────────────────────────────────╯
```

- One parameters form, one command line for all targets.
- A target with an active run is shown as busy and cannot be checked (one run per
  target, D-021).
- The run starts on every checked target; a target that fails to start does not affect
  the others (its tab shows why).

### The compare tab

A fleet run adds a `⧉ biolatency` tab at the front of the tab bar. It shows the run
across hosts; per-host tabs stay exactly as today.

```
╭ ⧉ biolatency │ local │ [db-01] ▶ │ db-02 ▶ │ + ───────────────────── 2 hosts · 00:42 ╮
│ host    state     elapsed  errors  dropped                                          │
│ db-01   running   00:42    0       0                                                │
│ db-02   running   00:42    3       0                                                │
│╭ @usecs · hist ─────────────────────────────────────────── p50 / p90 / p99 · max ──╮│
││db-01     count 18,204   p50 32–64 µs    p90 128–256 µs   p99 256–512 µs          ││
││db-02     count 17,950   p50 64–128 µs   p90 1–2 ms       p99 8–16 ms       ◀     ││
│╰───────────────────────────────────────────────────────────────────────────────────╯│
│╭ @syscalls · table ─────────────────────────────── top 10 by total ─ s sort ───────╮│
││key          total    db-01     db-02                                             ││
││postgres     9,412    1,207     8,205  ◀                                          ││
││sshd           204      101       103                                             ││
│╰───────────────────────────────────────────────────────────────────────────────────╯│
╰─────────────────────────────────────────────────────────────────────────────────────╯
```

- **Histograms:** one row per host with count and percentiles read from the buckets
  (a bucket range, not a made-up number, since that is all the data says). `Enter` on a
  host opens its tab. `m` switches to the merged histogram (buckets summed over hosts;
  log2 and lhist buckets line up because every host runs the same script).
- **Tables (maps):** key × host, sorted by total; `s` sorts by the selected host's column.
- **Single values:** one row per host.
- **Outliers:** `◀` marks a host whose p99 (hist) or value (table/value) is more than
  3× the median of the others. A hint, not a verdict; the threshold is a constant.
- Each host's latest snapshot is used; snapshots older than the host's print interval
  plus 2 s are dimmed (host is behind or stopped).

### Stopping, exporting, quitting

- In the compare tab `x` stops the whole fleet run (SIGINT everywhere, each host's END
  output still arrives). In a host tab `x` stops only that host, as today.
- `w` in the compare tab writes one export per host (as today, host in the name) plus
  a comparison report `bpfdeck-fleet-<script>-<UTC>.txt` with the tables above.
- Quitting lists all running targets, as today.

## How it works

Mostly app-level grouping over what exists:

- `App` gets `FleetRun { id, script_id, members: Vec<(TargetId, run_id)> }`. Starting it
  emits the existing `Cmd::StartRun` once per checked target; the executor and runner do
  not change. Each target keeps its own `Run`.
- The compare view is a pure `model/compare.rs` computed from the members' `Panels`:
  hist percentiles from buckets, merged buckets, key × host tables, outlier marks.
  Snapshot-tested like the other panels.
- Multi-connect: the dialog expands the host list (pure, tested: ranges, commas,
  duplicates, a cap of 20), then sends one `Cmd::Connect` per host; the executor already
  runs attempts concurrently. The interactive-auth handoffs are queued one at a time.
- Validation: nothing new; each connected target already validates every script. The
  matrix is only a view.
- SSH load: sshd allows 10 sessions per connection by default (`MaxSessions`). Per
  target bpfdeck uses at most 4 validation sessions + 1 run, so it stays under that.
  With 20 hosts, validating the bpftrace repo means ~1,900 short sessions spread over 20
  connections; the per-target worker limit already bounds it.

## What it does not do (v1 of fleet mode)

- No saved host groups or inventory files (the host list is typed, or pasted); a
  `~/.config/bpfdeck/hosts` could come later if typing lists gets old.
- No per-host parameters; one command line for all.
- No time alignment beyond "latest snapshot of each host"; hosts print on their own
  clocks.
- No hundreds of hosts (see Scale).

## Plan

| Step | Content | Size |
|---|---|---|
| F1 | Host list in the connect dialog (parse + expand), parallel checks, per-host result rows | M |
| F2 | Validation matrix in the Validation tab, disagreement count in the list | S |
| F3 | Target checklist in the confirmation, `FleetRun`, start/stop/quit on the group | M |
| F4 | `model/compare.rs` + compare tab: hist percentiles/merge, key × host tables, outliers | M |
| F5 | Fleet export, spec/README/D-entries, end-to-end with 3 sshd containers | S |

## Questions for you

1. **Scale:** 2–20 targets, one tab each, is the target. Enough, or do you need more?
2. **Starting a fleet run:** the target checklist in the run confirmation (no new key;
   the active target is preselected). Or a separate key that preselects all validated
   targets?
3. **Compare tab contents:** per-host percentiles for histograms (merged with `m`),
   key × host tables, `◀` outlier hint at 3× the median. Anything you'd look at first
   that is missing?
4. **Connecting many hosts:** a list typed into the Host field (with `{1..4}` ranges) and
   one sudo mode/password for all. Or should hosts come from somewhere else (e.g.
   `~/.ssh/config` Host entries to pick from, a hosts file)?
5. **Hosts where validation failed:** unchecked by default but allowed (a dry run can be
   wrong about a probe that appears later). Or never allowed?
