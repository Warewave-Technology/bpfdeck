# Remote execution over SSH, agentless — design

**Status: approved and implemented (R1–R5, 2026-09-29).** The owner's answers to the
questions at the end: sudo with a password yes; keys as proposed; `local` tab always shown,
red with "no bpftrace" when it has none; layout as proposed (D-021). Implementation
details that differ from or refine this text are in D-022 and D-023: one control socket
per target (`<id>-%C`), keepalives plus a 10 s `-O check`, host facts sent as the
session's script, and the runner un-ignoring SIGINT for its background job. To try it
against a real kernel: `tests/realhost/sshd.sh` (docs/real-kernel-testing.md).

An earlier version proposed an agent binary on the
host; the owner rejected it (D-020): *nothing may be installed on the target*. Any host
you can reach over SSH with admin rights must work, because this is for fixing problems
on servers you did not prepare.

## Goal (owner's requirements, 2026-09-29)

- No `--host` flag. bpfdeck starts as today (`bpfdeck <source>`); **inside the TUI a key
  opens "connect to a host"**: it asks for the address, runs its checks, connects.
- From then on **everything runs on that target**: validation, runs, exports.
- **Each target gets a results tab at the bottom** of the screen; what you run there shows
  up in that tab. Several hosts at once come later, so targets are tabs from day one.
- The **source given at startup applies to every target**: git stays git, a local path stays
  a local path, resolved on the laptop. The script being run is copied to the target at
  that moment and deleted when it is done.
- Nothing is installed on the target (D-020). It needs `bpftrace`, a POSIX `sh` and
  coreutils.

## User experience

### Screen

The browser stays on top; results move to a tabbed pane at the bottom, one tab per
target. `local` is the machine bpfdeck runs on.

```
╭ Scripts (93) ──────────────╮╭ 1 Info │ 2 Source │ 3 Validation ── tools/biolatency.bt ╮
│▸ src/  7                   ││Block I/O latency as a histogram.                        │
│▾ tools/  45                ││Status   ● runs on 10.0.3.14 (dry-run)                   │
│  ● biolatency  Block I/O…  ││Probes   ✓ tracepoint:block:block_rq_issue …             │
│  ● biosnoop   Block I/O…   ││                                                         │
╰────────────────────────────╯╰─────────────────────────────────────────────────────────╯
╭ local │ 10.0.3.14 ▶ │ db-02 ✓ │ + ─────────────────────────────── root via sudo · 5.14 ╮
│▶ biolatency.bt  running  00:12   probes 3  errors 0  dropped 0                          │
│╭ @usecs · hist ─────────────────────────────────────────────────── 12 updates · 00:12 ╮│
││[16, 32)   210 ████████████████████████████████████████                              ││
││[32, 64)    95 ██████████████████▏                                                   ││
│╰──────────────────────────────────────────────────────────────────────────────────────╯│
╰────────────────────────────────────────────────────────────────────────────────────────╯
 Enter run on 10.0.3.14 · c connect · < > target · ? help      10.0.3.14 · bpftrace v0.21.2
```

- The **active target** is the selected tab. The list's status glyphs, the Info and
  Validation tabs and `Enter` all refer to it (validated per target, cached per host).
- Tab glyphs: `▶` a run is active there, `✓` last run finished, `✗` connection lost, none
  when idle. `+` opens the connect dialog.
- The results pane shows the selected tab's run view (header, panels, log, exactly as
  today). `z` gives it the whole screen; `Esc` returns focus to the list, `o` to results.
- One run per target at a time (D-010 becomes "per target"); runs on different targets can
  go on together, each in its tab. Leaving a tab does not stop its run.

### Connecting: `c`

```
╭ Connect to a host ─────────────────────────────────────────────────────────╮
│ Host   10.0.3.14█                 IP, hostname, user@host or ~/.ssh/config alias │
│ Port   22                                                                   │
│ sudo   (•) automatic   ( ) root login   ( ) sudo with password: ••••••      │
│                                                                             │
│ ✓ ssh: connected as ops (key from ssh-agent)                     140 ms     │
│ ✓ host: Rocky Linux 9.4 · x86_64 · 5.14.0-427.13.1.el9_4                    │
│ ✓ shell: sh, mktemp, head -c, setsid                                        │
│ ✓ root: sudo -n works                                                       │
│ ✓ bpftrace v0.21.2 at /usr/bin/bpftrace · --dry-run supported               │
│ ✓ kernel: lockdown none · BTF present                                       │
│ … validating 93 scripts on 10.0.3.14  41/93                                 │
│                                                                             │
│ Enter connect · Esc cancel                                                  │
╰─────────────────────────────────────────────────────────────────────────────╯
```

- The checks run in order and stop at the first hard failure, with the reason and what to
  do (for example "sudo needs a password: pick 'sudo with password' or use root").
  Warnings (no `setsid`, lockdown, old bpftrace without `--dry-run`) do not block.
- On success a tab opens, becomes the active target, and validation of the whole source
  runs on it in the background (the list fills in as results arrive).
- **Interactive SSH authentication** (key passphrase, password, 2FA, unknown host key): the
  checks try non-interactive SSH first. If SSH needs you, the dialog offers "authenticate
  in the terminal": bpfdeck suspends the TUI exactly like `e` does for `$EDITOR`, runs
  `ssh … -fN` so SSH asks its own questions in the plain terminal, then resumes and
  continues the checks over the connection that was just opened. bpfdeck never sees
  SSH passwords or passphrases.
- A **sudo password**, if chosen, is typed into the dialog's masked field (see question 1).
- `d` in a tab disconnects that target (asks if a run is active there).

## How it works

Every operation (detect, validate, run) is one `ssh … server01 sh -s` session, multiplexed
over a single SSH connection. The remote command is always exactly `sh -s`; on stdin
bpfdeck sends a small, fixed POSIX sh program (the *runner*, ~50 lines, embedded in
bpfdeck, full text in the appendix) and then the data. The runner lives only in that
shell's memory: nothing is copied to disk except the script, in a private temp dir that
is removed when the session ends.

```
 laptop: bpfdeck                                server01
 ┌──────────────────────────┐  ssh (one        ┌──────────────────────────────────┐
 │ TUI, discovery, git,     │  ControlMaster   │ sh -s  ← runner (from stdin)      │
 │ editor, exports, spool   │  connection,     │   mktemp -d, script.bt            │
 │ runner/validator:        │  a session per   │   setsid bpftrace -f json … ──────┼─► stdout/stderr
 │   the child is `ssh`     │  operation)      │   stop: line or EOF on stdin      │   back to bpfdeck
 └──────────────────────────┘                  └──────────────────────────────────┘
```

Session protocol, in order. **Rule: never send data before the remote reader announced
itself.** dash reads scripts from a pipe in blocks, so anything sent early is swallowed by
the shell and lost.

1. *(only if not root)* sudo line, then wait for `bpfdeck-remote: root` on stderr:
   `exec sudo -n -- sh -c 'echo "bpfdeck-remote: root" >&2; exec sh -s'`.
   With a sudo password, the line first prints `bpfdeck-remote: sudo`; the password is sent
   only after that, and `sudo -S` reads it.
2. The runner program, ending in the single line `bpfdeck_main; exit $?`. bash reads line by
   line and runs a command as soon as the line is complete; with the call on the last
   line, nothing of the program is left on stdin to be mistaken for data.
3. Wait for `bpfdeck-remote: ready`, then send: `bpfdeck-remote 1`, the argv count, one argv
   entry per line, the script length, the script bytes.
4. Wait for `bpfdeck-remote: started`. From then on stdout is bpftrace's NDJSON and stderr
   its messages, exactly as for a local run.
5. Stop = write a line to stdin; the runner sends SIGINT to bpftrace's process group (END
   and the map dump run), SIGTERM after 5 s, SIGKILL after 2 s more. EOF on stdin (lost
   connection, bpfdeck exits) does the same. The session's exit status is bpftrace's.

The argv is built locally by `command::run_argv` / `dry_run_argv` / `probe_list_argv`, as
today, with the script path `script.bt` (the runner `cd`s into its temp dir). D-013 holds
unchanged: the script and all parameters come after `--`. Values containing a newline are
rejected (the only thing the line framing cannot carry).

## Evidence (spike, 2026-09-29)

OpenSSH + bpftrace 0.23.2 in a privileged container on the OrbStack kernel, reached over
`ssh` on localhost, users: root, `ops` (NOPASSWD sudo), `pw` (sudo with password).
Program: `interval:s:1 { @ticks = count(); } END { printf("END-RAN\n"); }`.

**Plain SSH (why a runner is needed):**

| Scenario | END + final dump | bpftrace afterwards |
|---|---|---|
| `ssh host bpftrace …`, local ssh terminated | lost | **still running as root** (orphan) |
| `ssh -tt host bpftrace …` (pty), local ssh terminated | lost | killed by SIGHUP |
| Program passed inline through `ssh … "sh -c '…'"` | — | quotes lost on the way: parse error |

**The runner (all passed):**

| Scenario | Result |
|---|---|
| dash and bash as `sh`, root and `ops` via sudo: stop by line | exit 0, END ran, final `@ticks` dumped, no processes or temp dirs left |
| Same four combinations: connection dropped (local ssh SIGKILL) | nothing left on the host |
| Script that exits by itself (`BEGIN { exit(); }`) | session ends in 0.3 s |
| `--dry-run` of a missing kprobe | non-zero exit, bpftrace's error on stderr |
| Parameters `two words`, `$(touch /tmp/pwned)`, `'; rm -rf / #`, `--unsafe`, `-o`, tab, unicode | all arrived byte for byte; no file created |
| Command ignoring SIGINT (with a background child) | SIGTERM after 5.1 s, exit 143, child gone too |
| Command ignoring SIGINT and SIGTERM | SIGKILL after 7.1 s, exit 137, child gone too |
| `pw` without a password (`sudo -n`) | `sudo: a password is required`, exit 1 |
| `pw` with the password via `sudo -S` | exit 0, END ran, nothing left |
| `pw` with a wrong password | `Sorry, try again.` seen in 2.5 s → bpfdeck gives up |
| SSH session cost, plain vs. multiplexed (ControlMaster) | 55 ms vs. 9 ms per session |

Bugs found and fixed during the spike (these are why the details above look the way they
do): dash block-reads the script (→ handshakes); bash executes the call line before
reading the rest (→ call on the last line); SIGKILL cannot pass through `sudo` and left a
root process holding the session (→ the runner itself runs as root, via the sudo line);
delayed `kill`s could hit a reused pid (→ signals come from the parent shell via traps);
dash's `kill` rejects `--` (→ `kill -INT -<pgid>`); a password sent right after the sudo
line was swallowed by dash (→ `bpfdeck-remote: sudo` marker first).

## What changes in bpfdeck

**Connection** (from the connect dialog): one SSH master per target,
`ssh -o ControlMaster=yes -o ControlPersist=… -o ControlPath=… -fN -- <host>`, first with
`BatchMode=yes`; only if that needs a human, the same command runs in the terminal while
the TUI is suspended (the `$EDITOR` handoff, input gate included), so SSH itself asks for a
passphrase, password, 2FA or host key confirmation. All later sessions reuse the master
with `-o BatchMode=yes` (never a prompt inside the TUI); `d`, a lost connection or quitting
closes it (`ssh -O exit`). The system `ssh` binary is
used, like `git` (D-004): keys, agent, `~/.ssh/config`, ProxyJump and known_hosts all apply.
The host argument is validated (no leading `-`) and placed after `--`. The control socket
lives in a private `0700` directory with a short path (unix socket paths are limited to
~104 bytes, so `%C` hashes, not host names).

**Privileges** (D-018): logged in as root → no sudo line. Otherwise `sudo -n`. If sudo needs
a password, see question 1.

**Targets:** the app keeps a list of targets (local + connected hosts), each with its
connection state, bpftrace info, validation results and run. The browser shows the active
target's validation; `Cmd::Validate` / `StartRun` / `StopRun` / `ExportRun` carry a target
id. **The source is resolved once, on the laptop**, and shared by all targets.

**Execution layer:** the executor dispatches each command to the target's backend (local
or SSH). For SSH targets:
- `capture()` (detect, dry-run, `-l`) spawns `ssh … sh -s` and speaks the session protocol;
  detection runs a fixed argv (`sh -c '<fixed probe of uname -r, id -u, lockdown,
  bpftrace --version/--help>'`, no user data).
- `runner::spawn` does the same; stop writes a line to the child's stdin instead of SIGINT.
  The remote side escalates; locally, if the session is still open 10 s after a stop, the
  ssh process is killed (EOF then stops the remote side anyway).
- Everything above it is unchanged: JSON parsing, coalescing, panels, log, the spool and
  exports (bpftrace's raw stdout arrives locally, byte for byte).
- The validator's cache key already has bpftrace version and kernel; the host is added.

**UI:** the layout, connect dialog and tabs above; the run confirmation says **Run on
10.0.3.14 as root**; SSH errors show ssh's own message; on a lost connection the tab turns
`✗` and its run view says the run was stopped on the host.

## Security

- Nothing is installed or left on the host: the runner is in memory, the temp dir (`0700`)
  is removed on exit, and a dropped connection stops bpftrace cleanly (tested).
- The only remote command is the constant `sh -s`. User data (scripts, parameters) goes
  through stdin with length/line framing, never through a shell command line.
- The runner is a fixed text in the bpfdeck binary, reviewable (appendix), no templating.
- SSH host key checking and auth policy stay with the user's configuration; bpfdeck never
  weakens them. No listening sockets are opened on either side.
- The trust model is unchanged: scripts run as root on the host, after a confirmation that
  names the host; `--unsafe` is per-run opt-in (D-009).
- A sudo password, if supported (question 1), is read without echo, kept only in memory
  for the session, sent only over the encrypted SSH channel and only after the
  `bpfdeck-remote: sudo` marker, and never written anywhere or passed on a command line.

## Failure modes

| Case | Behavior |
|---|---|
| Host unreachable, auth fails, host key changed | The failing check in the connect dialog, with ssh's own message |
| Master connection dies later | "connection lost"; runs stopped on the host by EOF; a reconnect action |
| sudo needs a password and none was given | Clear message: use root, NOPASSWD, or the password option |
| Wrong sudo password | Detected from `Sorry, try again.`; the dialog asks again |
| No `bpftrace` on the host | The check fails with "not found in PATH"; the dialog then offers a bpftrace path field |
| Kernel lockdown on the host | The red banner, for that host |
| A handshake marker never arrives (odd shell, broken sudo config) | Timeout, the session's stderr shown |
| No `setsid` on the host | Falls back to signalling bpftrace alone (children of `system()` could survive) |

## Testing

- The runner and the protocol are exercised without network through a **fake `ssh`**
  (test hook `--ssh <path>`: a script that ignores the host and runs `sh -s` locally), with
  the fake bpftrace. In CI (Ubuntu) `sh` is dash, so both read-ahead quirks are covered;
  bash is tested by pointing the fake at `bash -s`.
- `tests/realhost` gets the spike's sshd image (root, NOPASSWD user, password user) for
  real SSH end-to-end runs.

## Plan

| Step | Content | Size |
|---|---|---|
| R1 | Targets in the app model (local only at first): per-target validation, run and results tab; bottom results pane with tabs; no behavior change for local use | M |
| R2 | Embedded runner + session protocol (handshakes, framing, timeouts); SSH backend for capture/run; fake-ssh tests on dash and bash | M |
| R3 | Connect dialog: host/port/sudo fields, the checks, terminal handoff for interactive auth, master lifecycle, `d` disconnect | M |
| R4 | Per-target validation on connect, confirmation naming the host, lost-connection handling, exports per tab | S |
| R5 | Real end-to-end via the sshd container (root, NOPASSWD, password sudo); spec §3/§5/§6, README, D-entries | S |

Multi-host (later): run the selected script on several targets at once, each result in its
tab; a validation matrix (script × host) in the Validation tab.

## Questions for you

1. **sudo with a password:** a masked field in the connect dialog, the password kept in
   memory for as long as that target is connected and sent over the SSH channel to
   `sudo -S` for each operation (the spike shows this works; sudo's own timestamp does not
   carry over between non-interactive sessions). OK, or root / NOPASSWD only?
2. **Keys:** `c` connect (also `+` on the tab bar), `<` `>` switch target tabs, `d` disconnect
   the selected target. OK?
3. **`local` tab:** always present (on a laptop without bpftrace it just shows "no bpftrace"),
   or only when bpftrace exists locally?
4. **Layout:** browser on top, results pane at the bottom (`z` to maximize) as in the mockup?

## Appendix: the runner, verbatim (as tested)

```sh
# bpfdeck remote runner, sent over `ssh host sh -s` (after an optional fixed sudo line that
# turns the session into a root `sh -s`): nothing is installed on the host.
# Protocol on stdin, after the "ready" handshake: "bpfdeck-remote 1", argv count, one argv
# entry per line, script length, script bytes.
# After "started", any line or EOF on stdin stops the command: SIGINT, then SIGTERM
# after 5 s, SIGKILL after 2 s more, to the command's whole process group. Signals are
# sent by this shell (the command's parent), so the pid cannot have been reused. The
# exit status is the command's.
bpfdeck_main() {
  umask 077
  echo "bpfdeck-remote: ready" >&2
  IFS= read -r version || exit 71
  [ "$version" = "bpfdeck-remote 1" ] || { echo "bpfdeck-remote: bad header" >&2; exit 71; }
  IFS= read -r n || exit 71
  set --
  i=0
  while [ "$i" -lt "$n" ]; do
    IFS= read -r a || exit 71
    set -- "$@" "$a"
    i=$((i + 1))
  done
  IFS= read -r len || exit 71
  d=$(mktemp -d "${TMPDIR:-/tmp}/bpfdeck.XXXXXX") || exit 72
  trap 'rm -rf "$d"' EXIT
  if [ "$len" -gt 0 ]; then
    head -c "$len" > "$d/script.bt" || exit 72
  fi
  cd "$d" || exit 72
  # Own process group (like the local runner), so children of bpftrace are signalled too.
  if command -v setsid > /dev/null 2>&1; then
    setsid "$@" < /dev/null &
    g="-$!"
  else
    "$@" < /dev/null &
    g="$!"
  fi
  p=$!
  trap 'kill -INT "$g" 2>/dev/null' USR1
  trap 'kill -TERM "$g" 2>/dev/null' USR2
  trap 'kill -KILL "$g" 2>/dev/null' ALRM
  echo "bpfdeck-remote: started" >&2
  exec 3<&0
  ( IFS= read -r _ <&3; kill -USR1 $$; sleep 5; kill -USR2 $$; sleep 2; kill -ALRM $$ ) > /dev/null 2>&1 &
  w=$!
  while :; do
    wait "$p"
    c=$?
    kill -0 "$p" 2>/dev/null || break
  done
  kill "$w" 2>/dev/null
  kill -KILL "$g" 2>/dev/null
  exit "$c"
}
bpfdeck_main; exit $?
```
