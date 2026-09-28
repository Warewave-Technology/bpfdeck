# Remote execution over SSH — design proposal

**Status: proposal, awaiting the owner's approval. Nothing here is implemented.**
Questions to answer are at the end. Until approved, spec §3 still lists remote execution
as out of scope.

## Goal

From a laptop or jump host: browse a local directory or git repo of scripts, and validate
and run them **on a remote Linux host**, with the same TUI: live panels, the exit-time
dump, export. The remote host needs only `bpftrace` and an SSH login with root (D-018).

Non-goals for the first cut: running on several hosts at once (fleet), resident daemons or
network listeners, password handling.

## Options

| | How | Verdict |
|---|---|---|
| **A** | Copy the static binary, `ssh -t host sudo bpfdeck <source>`: the whole TUI runs remotely | Works **today** (release binaries are static). But scripts must be on the remote (or a git URL the remote can fetch), `$EDITOR` and exports are remote, one invocation per host. Worth documenting now; not the feature. |
| **B** | Local bpfdeck (UI, discovery, git, editor, exports) talks to `bpfdeck agent` on the remote over SSH stdio; the agent runs bpftrace | **Recommended.** Uses the existing Msg/Cmd split; keeps every safety property; testable without network. |
| **C** | Local bpfdeck spawns `ssh host sudo bpftrace …` directly | **Rejected**: measured below — stops lose the exit dump, a dropped connection leaves bpftrace running as root, and passing scripts/params through the remote shell breaks. |

## Evidence (spike, 2026-09-29)

Throwaway container with OpenSSH + bpftrace 0.23.2 on the OrbStack kernel, reached over
`ssh` on localhost. Program: `interval:s:1 { @ticks = count(); } END { printf("END-RAN\n"); }`,
stopped after 3 s.

| Scenario | END ran + final map dumped | bpftrace afterwards |
|---|---|---|
| C-style `ssh -T host bpftrace …`, local ssh terminated | no | **still running, as root** (orphan) |
| Same with a pty (`ssh -tt`) | no | killed (SIGHUP) |
| Wrapper reading stdin; "stop" line → SIGINT | **yes**, exit 0 | gone |
| Same wrapper, connection dropped (local ssh SIGKILL) | **yes** (EOF on stdin → SIGINT) | gone |

A first attempt that passed the program inline through `ssh … "sh -c '…'"` lost its quotes
on the way (`printf("END-RAN\n")` arrived as `printf(END-RANn)` → parse error). That is the
remote version of D-013: user data must never travel inside a remote command line.

Takeaway: control must flow over the SSH channel's **stdin**, handled by a process on the
remote that owns bpftrace. That is the agent.

## Design (option B)

```
 local bpfdeck                                   remote host (root)
 ┌──────────────────────────────┐   ssh stdio   ┌──────────────────────────────┐
 │ TUI, App (unchanged)         │   NDJSON      │ bpfdeck agent                │
 │ discovery, metadata, git     │ ◄───────────► │  validator (dry-run/-l)      │
 │ editor, exports              │               │  runner (pgroup, INT→TERM→KILL)
 │ Executor → RemoteBackend ────┼── ssh ────────┤  coalescer, spool            │
 └──────────────────────────────┘               │  → bpftrace                  │
                                                └──────────────────────────────┘
```

**The seam already exists.** Every bpftrace side effect is a `Cmd` handled by the executor:
`DetectEnv`, `Validate`, `StartRun`, `StopRun`, `ExportRun` (spool). A `Backend` trait with
two implementations — `LocalBackend` (today's code, moved) and `RemoteBackend` (protocol
client) — leaves `App`, the model and the widgets unchanged apart from showing the host.

**The agent is the same static binary** (`bpfdeck agent`), reusing the tested pieces:
`command::run_argv` (argv built on the remote, D-013 intact), `runner` (process group,
SIGINT → SIGTERM → SIGKILL), `validate` (strategies, cache), `coalesce` (now also absorbs a
slow network), the spool. It lives exactly as long as the SSH session: no daemon, no port.

### Protocol

NDJSON over the SSH channel, one message per line, request ids where replies are needed.
All user data (script text, parameters) travels here, never on a command line.

| local → agent | agent → local |
|---|---|
| `hello {version}` | `hello {version, arch, kernel}` |
| `detect {}` | `env {host: SystemInfo, bpftrace: BpftraceInfo \| error}` |
| `validate {id, script, positional_count, probes}` | `validated {id, content_hash, validation}` |
| `run {run_id, script, positional, named, allow_unsafe}` | `run_started {run_id}` / `run_failed {run_id, reason}` |
| `stop {run_id}` | `run_batch {run_id, events, dropped}` (coalesced; exit last) |
| `spool {run_id}` | `spool_chunk {run_id, data, last}` |
| (stdin EOF = disconnect) | `error {message}` |

Needs `serde` derives on `OutputMsg`, `RunEvent`, `Validation`, `SystemInfo`,
`BpftraceInfo` (serde is already a dependency). The agent writes each script to a private
`0700` temp dir; `validate`/`run` refer to that file.

**Disconnect = stop.** EOF on stdin makes the agent stop any run gracefully (SIGINT, END
runs) and exit, cleaning its temp dir — the "dropped connection" row above, without orphans.

### Connecting and deploying the agent

- `bpfdeck --host [user@]host <source>`. The system `ssh` binary is used, as with `git`
  (D-004): the user's keys, agent, `~/.ssh/config` aliases, jump hosts and known_hosts all
  just work. `-o BatchMode=yes` so nothing prompts inside the TUI; auth errors are shown
  with a hint to test `ssh host` by hand.
- Only fixed remote command strings, with nothing user-supplied in them: `uname -m`,
  `sha256sum <cache path>`, `cat > <cache path>.tmp`, `mv`, `<cache path> agent`. The cache
  path is `~/.cache/bpfdeck/agent/bpfdeck-<sha256[..16]>` (hex only).
- Which binary to push: the running one when the remote arch matches; otherwise
  `--agent-binary <path>`, or a `bpfdeck-<arch>` file next to the local binary.
- Upload once per version: stream to a temp name, verify the sha256 on the remote, rename,
  `chmod 700`. Later connects just check the hash.
- Privileges (D-018): log in as root, or `sudo -n` (NOPASSWD). bpfdeck never asks for or
  forwards passwords. `--remote-sudo auto|always|never`, where auto means "use sudo if not root".

### What changes in the UI

- Status bar: `server01 (aarch64) · bpftrace v0.21.2 · 5.14.0-427… · root`.
- Connection states: connecting / uploading agent / ready / lost. On loss, the run view
  says the run was stopped on the host, and a reconnect action is offered.
- Run confirmation names the host: **Run on server01 as root**, next to the exact command.
- Validation results are per host (the cache key already has bpftrace version and kernel).
- Exports are written locally; the raw NDJSON is fetched from the agent's spool.
- `e` (editor), discovery, git: unchanged, all local.

### Security

- The trust model is unchanged: scripts run as root on the target, only after the
  confirmation (which now names the host), and `--unsafe` is a per-run opt-in (D-009).
- No shell interpolation of user data on the remote (see the spike).
- The pushed binary is verified by content hash and kept in a `0700` directory.
- No listening sockets, no persistent processes: everything rides one SSH session.
- Host key checking stays with the user's SSH config; bpfdeck never weakens it.

### Failure modes

| Case | Behavior |
|---|---|
| SSH auth fails / host unreachable | Error screen with ssh's stderr and "try `ssh host` in a shell" |
| sudo needs a password | Explain NOPASSWD or root login; nothing is prompted |
| No agent binary for the remote arch | Error naming the arch and `--agent-binary` |
| bpftrace missing / lockdown on the remote | Same as local: `?` / banner, but for that host |
| Connection drops mid-run | Agent stops bpftrace gracefully and exits; UI shows "lost; run stopped on host" |
| Slow link | Agent-side coalescing; the `dropped` counter shows text lines lost |

### Testing

- Protocol encode/decode round-trip tests.
- Agent tests in-process over pipes, with the fake bpftrace (no SSH).
- `RemoteBackend` against a **fake `ssh`** (test hook `--ssh <path>`) that runs the agent
  locally: the full remote path, with no network.
- End to end over real SSH: `tests/realhost` gets the spike's sshd image.

### Plan

| Step | Content | Size |
|---|---|---|
| R1 | `Backend` trait; today's executor code becomes `LocalBackend`. No behavior change | S |
| R2 | serde + protocol + `bpfdeck agent` over stdio; tests over pipes | M |
| R3 | SSH transport: connect, arch check, upload + verify, hello; fake-ssh tests | M |
| R4 | UI: host badge, connection states, errors, confirmation; export fetch | M |
| R5 | Real end-to-end with the sshd container; spec/README; D-entries | S |

Later, not in this proposal: fleet mode (validate on N hosts as a matrix, run on one), an
agent download from GitHub releases.

## Questions for you

1. **Approach:** B (agent over SSH) as proposed, or only document A for now?
2. **Privileges:** root login or NOPASSWD `sudo -n` only, never password prompts — OK?
3. **Other architectures:** `--agent-binary` / a sibling `bpfdeck-<arch>` file. Should the
   release tarballs ship both architectures to make that automatic?
4. **Scope:** one host per session first, fleet later — OK?
5. **Spec:** move "Remote execution over SSH" from out of scope (§3) to v1.1 once you approve.
