//! Run bpftrace, stream its output as [`RunEvent`]s, stop it gracefully (spec §5.4, §6.5).
//!
//! The child gets its own process group; every signal goes to the whole group. A stop
//! sends SIGINT (bpftrace runs END and dumps its maps), then SIGTERM, then SIGKILL on
//! timeouts. `Exited` is sent only after stdout and stderr are drained, so the exit-time
//! dump always arrives before it.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nix::sys::signal::Signal;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::json::{self, OutputMsg};
use super::signal_group;

/// Bound for the event channel (spec §6.5).
pub const CHANNEL_CAPACITY: usize = 4096;
/// After the child exits, how long to wait for its pipes to close before giving up.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    Output(OutputMsg),
    /// One stderr line (errors and warnings are plain text).
    Stderr(String),
    /// Always the last event of a run.
    Exited(RunExit),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunExit {
    pub code: Option<i32>,
    /// Signal that terminated the process, if any.
    pub signal: Option<i32>,
    /// Set when SIGINT was not enough and the run had to be escalated.
    pub forced: Option<Signal>,
    /// Waiting for the process failed (rare; the process state is unknown).
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Escalation {
    /// SIGINT → SIGTERM after this long.
    pub after_int: Duration,
    /// SIGTERM → SIGKILL after this long.
    pub after_term: Duration,
}

impl Default for Escalation {
    fn default() -> Self {
        Self {
            after_int: Duration::from_secs(5),
            after_term: Duration::from_secs(2),
        }
    }
}

/// Keep a copy of bpftrace's raw stdout in a file, for exporting a run (M6). Bounded:
/// past `cap` bytes nothing more is written and the handle reports truncation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spool {
    pub path: PathBuf,
    pub cap: u64,
}

pub const SPOOL_CAP: u64 = 256 * 1024 * 1024;

struct SpoolWriter {
    out: BufWriter<File>,
    written: u64,
    cap: u64,
    truncated: Arc<AtomicBool>,
}

impl SpoolWriter {
    fn write(&mut self, raw: &[u8]) {
        if self.truncated.load(Ordering::Relaxed) {
            return;
        }
        let len = raw.len() as u64;
        if self.written + len > self.cap || self.out.write_all(raw).is_err() {
            self.truncated.store(true, Ordering::Relaxed);
            return;
        }
        self.written += len;
    }
}

/// Handle to a running bpftrace. Dropping it kills the process group.
pub struct RunHandle {
    pgid: u32,
    stop: Option<oneshot::Sender<()>>,
    finished: Arc<AtomicBool>,
    spool_truncated: Arc<AtomicBool>,
}

impl RunHandle {
    pub fn pgid(&self) -> u32 {
        self.pgid
    }

    /// Start the graceful stop sequence. Idempotent.
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// The spool hit its cap (or could not be written): the raw copy is incomplete.
    pub fn spool_truncated(&self) -> bool {
        self.spool_truncated.load(Ordering::Relaxed)
    }
}

impl Drop for RunHandle {
    fn drop(&mut self) {
        if !self.is_finished() {
            signal_group(self.pgid, Signal::SIGKILL);
        }
    }
}

/// Spawn `argv` (see `command::run_argv`) and stream events into `tx`. With `spool`, raw
/// stdout is also copied to that file (a spool that can't be created is just skipped).
/// Must be called inside a tokio runtime. The child inherits bpfdeck's *ignored* signals,
/// so install signal handlers (which reset to default on exec) before spawning.
pub fn spawn(
    argv: &[OsString],
    tx: mpsc::Sender<RunEvent>,
    escalation: Escalation,
    spool: Option<&Spool>,
) -> io::Result<RunHandle> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;
    let mut child = Command::new(program)
        .args(args)
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pgid = child
        .id()
        .ok_or_else(|| io::Error::other("child exited before its pid was read"))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(io::Error::other("child pipes missing"));
    };
    Ok(start(
        Started {
            child,
            pgid,
            stdout: Box::new(stdout),
            stderr: Box::new(stderr),
            stop: StopMode::SignalGroup,
            remote: false,
        },
        tx,
        escalation,
        spool,
    ))
}

/// How a graceful stop is requested.
pub enum StopMode {
    /// SIGINT → SIGTERM → SIGKILL to the local process group (a local bpftrace).
    SignalGroup,
    /// Write a line to the child's stdin; the remote runner escalates by itself
    /// (docs/design-remote.md). Held for the whole run: closing it also means "stop".
    StdinLine(ChildStdin),
}

/// A spawned process whose output is bpftrace's (locally, or through `ssh` once the
/// remote handshake is done).
pub struct Started {
    pub child: Child,
    pub pgid: u32,
    pub stdout: Box<dyn AsyncRead + Unpin + Send>,
    pub stderr: Box<dyn AsyncRead + Unpin + Send>,
    pub stop: StopMode,
    /// The exit status is the remote shell's: 128+N means "killed by signal N".
    pub remote: bool,
}

/// After a stop line, how long the remote side gets (its own INT→TERM→KILL takes 7 s)
/// before the local `ssh` is killed, which ends the session and stops it anyway (EOF).
const REMOTE_STOP_GRACE: Duration = Duration::from_secs(3);

/// Stream a started process: parse stdout, forward stderr, spool, stop, report the exit.
pub fn start(
    started: Started,
    tx: mpsc::Sender<RunEvent>,
    escalation: Escalation,
    spool: Option<&Spool>,
) -> RunHandle {
    let (handle, parts) = handle(started.pgid);
    start_with(started, tx, escalation, spool, parts);
    handle
}

/// Like [`start`] for a process that first has to get ready (the remote handshake):
/// the handle exists right away, so a stop or drop during `ready` is not lost. If `ready`
/// fails, its message becomes a stderr line and the run exits with that error.
pub fn start_deferred(
    pgid: u32,
    tx: mpsc::Sender<RunEvent>,
    escalation: Escalation,
    spool: Option<Spool>,
    ready: impl Future<Output = Result<Started, String>> + Send + 'static,
) -> RunHandle {
    let (handle, mut parts) = handle(pgid);
    tokio::spawn(async move {
        let started = tokio::select! {
            started = ready => started,
            // Stopped or dropped before it ran: nothing to wait for.
            _ = &mut parts.stop_rx => Err("stopped before it started".to_string()),
        };
        match started {
            Ok(started) => start_with(started, tx, escalation, spool.as_ref(), parts),
            Err(e) => {
                signal_group(pgid, Signal::SIGKILL);
                parts.finished.store(true, Ordering::Release);
                let _ = tx.send(RunEvent::Stderr(e.clone())).await;
                let exit = RunExit {
                    code: None,
                    signal: None,
                    forced: None,
                    error: Some(e),
                };
                let _ = tx.send(RunEvent::Exited(exit)).await;
            }
        }
    });
    handle
}

/// The supervisor's ends of a [`RunHandle`].
struct HandleParts {
    stop_rx: oneshot::Receiver<()>,
    finished: Arc<AtomicBool>,
    spool_truncated: Arc<AtomicBool>,
}

fn handle(pgid: u32) -> (RunHandle, HandleParts) {
    let (stop_tx, stop_rx) = oneshot::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let spool_truncated = Arc::new(AtomicBool::new(false));
    (
        RunHandle {
            pgid,
            stop: Some(stop_tx),
            finished: finished.clone(),
            spool_truncated: spool_truncated.clone(),
        },
        HandleParts {
            stop_rx,
            finished,
            spool_truncated,
        },
    )
}

fn start_with(
    started: Started,
    tx: mpsc::Sender<RunEvent>,
    escalation: Escalation,
    spool: Option<&Spool>,
    parts: HandleParts,
) {
    let Started {
        child,
        pgid,
        stdout,
        stderr,
        stop,
        remote,
    } = started;
    let HandleParts {
        stop_rx,
        finished,
        spool_truncated,
    } = parts;
    let spool = spool.and_then(|s| {
        let file = File::create(&s.path).ok()?;
        Some(SpoolWriter {
            out: BufWriter::new(file),
            written: 0,
            cap: s.cap,
            truncated: spool_truncated.clone(),
        })
    });
    let readers = vec![
        tokio::spawn(read_lines(stdout, tx.clone(), spool, |line| {
            json::parse_line(&line)
                .into_iter()
                .map(RunEvent::Output)
                .collect()
        })),
        tokio::spawn(read_lines(stderr, tx.clone(), None, |line| {
            if line.trim().is_empty() {
                Vec::new()
            } else {
                vec![RunEvent::Stderr(line)]
            }
        })),
    ];
    tokio::spawn(supervise(
        child,
        Stopper {
            pgid,
            mode: stop,
            escalation,
            remote,
        },
        stop_rx,
        readers,
        tx,
        finished,
    ));
}

struct Stopper {
    pgid: u32,
    mode: StopMode,
    escalation: Escalation,
    remote: bool,
}

async fn supervise(
    mut child: Child,
    mut stopper: Stopper,
    mut stop_rx: oneshot::Receiver<()>,
    readers: Vec<JoinHandle<()>>,
    tx: mpsc::Sender<RunEvent>,
    finished: Arc<AtomicBool>,
) {
    let pgid = stopper.pgid;
    let mut forced = None;
    let status = tokio::select! {
        status = child.wait() => status,
        stop = &mut stop_rx => {
            if stop.is_ok() {
                match &mut stopper.mode {
                    StopMode::SignalGroup => {
                        stop_gracefully(&mut child, pgid, stopper.escalation, &mut forced).await
                    }
                    StopMode::StdinLine(stdin) => {
                        let _ = stdin.write_all(b"stop\n").await;
                        let _ = stdin.flush().await;
                        let e = stopper.escalation;
                        let budget = e.after_int + e.after_term + REMOTE_STOP_GRACE;
                        match tokio::time::timeout(budget, child.wait()).await {
                            Ok(status) => status,
                            Err(_) => {
                                forced = Some(Signal::SIGKILL);
                                signal_group(pgid, Signal::SIGKILL);
                                child.wait().await
                            }
                        }
                    }
                }
            } else {
                // Handle dropped: its Drop already sent SIGKILL.
                child.wait().await
            }
        }
    };
    // The stop line's stdin (remote) stays open until here: closing it means "stop".
    drop(stopper.mode);
    finished.store(true, Ordering::Release);
    // bpftrace is gone; take down anything it left in its group (e.g. `-c` commands,
    // `system()` children) so they don't hold the pipes open.
    signal_group(pgid, Signal::SIGKILL);

    for mut reader in readers {
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut reader).await.is_err() {
            reader.abort();
        }
    }
    let mut exit = exit_of(status, forced);
    if stopper.remote {
        remote_exit(&mut exit);
    }
    let _ = tx.send(RunEvent::Exited(exit)).await;
}

/// The remote shell exits with 128+N when bpftrace was killed by signal N; report it
/// like a local signal death, and SIGTERM/SIGKILL as escalations of our stop.
fn remote_exit(exit: &mut RunExit) {
    if let Some(code) = exit.code
        && (129..160).contains(&code)
    {
        let sig = code - 128;
        exit.code = None;
        exit.signal = Some(sig);
        if exit.forced.is_none() {
            exit.forced = match sig {
                15 => Some(Signal::SIGTERM),
                9 => Some(Signal::SIGKILL),
                _ => None,
            };
        }
    }
}

async fn stop_gracefully(
    child: &mut Child,
    pgid: u32,
    escalation: Escalation,
    forced: &mut Option<Signal>,
) -> io::Result<ExitStatus> {
    signal_group(pgid, Signal::SIGINT);
    if let Ok(status) = tokio::time::timeout(escalation.after_int, child.wait()).await {
        return status;
    }
    *forced = Some(Signal::SIGTERM);
    signal_group(pgid, Signal::SIGTERM);
    if let Ok(status) = tokio::time::timeout(escalation.after_term, child.wait()).await {
        return status;
    }
    *forced = Some(Signal::SIGKILL);
    signal_group(pgid, Signal::SIGKILL);
    child.wait().await
}

fn exit_of(status: io::Result<ExitStatus>, forced: Option<Signal>) -> RunExit {
    match status {
        Ok(status) => RunExit {
            code: status.code(),
            signal: status.signal(),
            forced,
            error: None,
        },
        Err(e) => RunExit {
            code: None,
            signal: None,
            forced,
            error: Some(e.to_string()),
        },
    }
}

/// Read `\n`-terminated lines (lossy UTF-8, so odd bytes never end the stream) and
/// forward the events `f` makes of them. Keeps draining if the receiver is gone, so the
/// child never blocks on a full pipe.
async fn read_lines(
    stream: impl AsyncRead + Unpin,
    tx: mpsc::Sender<RunEvent>,
    mut spool: Option<SpoolWriter>,
    f: impl Fn(String) -> Vec<RunEvent>,
) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if let Some(spool) = &mut spool {
                    spool.write(&buf);
                    // Flush whenever we caught up with the pipe: cheap under a flood,
                    // and an export mid-run then has everything read so far.
                    if reader.buffer().is_empty() {
                        let _ = spool.out.flush();
                    }
                }
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim_end_matches(['\n', '\r']).to_string();
                for event in f(line) {
                    let _ = tx.send(event).await;
                }
            }
        }
    }
    if let Some(spool) = &mut spool {
        let _ = spool.out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::super::command::{self, NamedArg, NamedValue, RunArgs};
    use super::super::json::{HistSeries, TextKind};
    use super::super::testutil::*;
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::Path;

    const FAST: Escalation = Escalation {
        after_int: Duration::from_millis(300),
        after_term: Duration::from_millis(300),
    };

    fn argv(script: &Path) -> Vec<OsString> {
        let args = RunArgs {
            script,
            positional: &[],
            named: &[],
            allow_unsafe: false,
        };
        command::run_argv(&fake(), &args).expect("argv")
    }

    /// Collect events until `Exited`, with a hard deadline so a bug can't hang the suite.
    async fn collect(rx: &mut mpsc::Receiver<RunEvent>) -> (Vec<RunEvent>, RunExit) {
        let mut events = Vec::new();
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("run did not finish in time")
                .expect("channel closed before Exited");
            match ev {
                RunEvent::Exited(exit) => return (events, exit),
                other => events.push(other),
            }
        }
    }

    /// Wait until the stream has produced `n` events (so the replay is done).
    async fn wait_for(rx: &mut mpsc::Receiver<RunEvent>, n: usize) -> Vec<RunEvent> {
        let mut events = Vec::new();
        while events.len() < n {
            let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("timed out waiting for events")
                .expect("channel closed");
            events.push(ev);
        }
        events
    }

    /// Killed orphans linger as zombies until init reaps them; give that a moment.
    async fn group_alive(pgid: u32) -> bool {
        for _ in 0..40 {
            if nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid as i32), None).is_err() {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        true
    }

    #[tokio::test]
    async fn streams_then_captures_exit_time_dump_on_stop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = script(
            &dir,
            "t.bt",
            "// fake: replay=session_mixed.ndjson\n// fake: stderr=WARNING: fake\nBEGIN {}\n",
        );
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let mut handle = spawn(&argv(&s), tx, FAST, None).expect("spawn");

        // 5 stdout messages + 1 stderr line.
        let first = wait_for(&mut rx, 6).await;
        assert!(first.contains(&RunEvent::Stderr("WARNING: fake".into())));
        let outputs: Vec<_> = first
            .iter()
            .filter_map(|e| match e {
                RunEvent::Output(m) => Some(m),
                _ => None,
            })
            .collect();
        assert_eq!(outputs.len(), 5);
        assert_eq!(outputs[0], &OutputMsg::AttachedProbes(3));
        assert!(matches!(
            outputs[1],
            OutputMsg::Text {
                kind: TextKind::Printf,
                ..
            }
        ));

        handle.stop();
        handle.stop(); // idempotent
        let (rest, exit) = collect(&mut rx).await;
        assert!(
            matches!(rest.as_slice(), [RunEvent::Output(OutputMsg::Hist { name, series: HistSeries::Single(_) })] if name == "@final"),
            "{rest:?}"
        );
        assert!(exit.code == Some(0), "{exit:?}");
        assert_eq!(exit.forced, None);
        assert!(handle.is_finished());
    }

    #[tokio::test]
    async fn natural_exit_code_and_not_json_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = script(
            &dir,
            "t.bt",
            "// fake: replay=printf.ndjson\n// fake: exit=3\nBEGIN {}\n",
        );
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let _handle = spawn(&argv(&s), tx, FAST, None).expect("spawn");
        let (events, exit) = collect(&mut rx).await;
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(exit.code, Some(3));
        assert!(exit.code != Some(0));
    }

    #[tokio::test]
    async fn escalates_to_term_then_kill() {
        let dir = tempfile::tempdir().expect("tempdir");
        for (directives, want) in [
            ("// fake: ignore_int=1\n", Signal::SIGTERM),
            ("// fake: ignore_int=1\n// fake: ignore_term=1\n", Signal::SIGKILL),
        ] {
            let s = script(
                &dir,
                "t.bt",
                &format!("{directives}// fake: replay=printf.ndjson\nBEGIN {{}}\n"),
            );
            let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
            let mut handle = spawn(&argv(&s), tx, FAST, None).expect("spawn");
            wait_for(&mut rx, 1).await; // traps are installed before the replay
            handle.stop();
            let (_, exit) = collect(&mut rx).await;
            assert_eq!(exit.forced, Some(want), "{directives}");
            assert_eq!(exit.signal, Some(want as i32), "{exit:?}");
        }
    }

    #[tokio::test]
    async fn whole_process_group_is_gone_after_exit() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The background child ignores SIGINT (async commands in sh do) and would
        // otherwise outlive bpftrace and keep stdout open.
        let s = script(
            &dir,
            "t.bt",
            "// fake: child=1\n// fake: replay=printf.ndjson\nBEGIN {}\n",
        );
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let mut handle = spawn(&argv(&s), tx, FAST, None).expect("spawn");
        let pgid = handle.pgid();
        wait_for(&mut rx, 1).await;
        handle.stop();
        let (_, exit) = collect(&mut rx).await;
        assert!(exit.code == Some(0), "{exit:?}");
        assert!(!group_alive(pgid).await, "process group {pgid} still has members");
    }

    #[tokio::test]
    async fn dropping_the_handle_kills_the_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = script(&dir, "t.bt", "// fake: replay=printf.ndjson\nBEGIN {}\n");
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let handle = spawn(&argv(&s), tx, FAST, None).expect("spawn");
        wait_for(&mut rx, 1).await;
        drop(handle);
        let (_, exit) = collect(&mut rx).await;
        assert_eq!(exit.signal, Some(Signal::SIGKILL as i32));
    }

    #[tokio::test]
    async fn argv_reaches_bpftrace_verbatim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("argv.log");
        let s = script(
            &dir,
            "t.bt",
            &format!(
                "// fake: argv_log={}\n// fake: exit=0\nBEGIN {{}}\n",
                log.display()
            ),
        );
        let positional = vec![
            "two words".to_string(),
            "$(touch pwned)".to_string(),
            "-o".to_string(),
        ];
        let named = vec![NamedArg {
            name: "sep".into(),
            value: NamedValue::Value("'; rm -rf / #".into()),
        }];
        let args = RunArgs {
            script: &s,
            positional: &positional,
            named: &named,
            allow_unsafe: false,
        };
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let _handle =
            spawn(&command::run_argv(&fake(), &args).expect("argv"), tx, FAST, None).expect("spawn");
        let (_, exit) = collect(&mut rx).await;
        assert!(exit.code == Some(0), "{exit:?}");
        let logged = std::fs::read_to_string(&log).expect("log");
        let logged: Vec<_> = logged.lines().skip(1).collect(); // skip $0
        assert_eq!(
            logged,
            vec![
                s.to_str().expect("utf8"),
                "two words",
                "$(touch pwned)",
                "-o",
                "--sep='; rm -rf / #",
                "--end--"
            ]
        );
        assert!(!dir.path().join("pwned").exists());
    }

    #[tokio::test]
    async fn spool_keeps_raw_stdout_and_respects_its_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = script(
            &dir,
            "t.bt",
            "// fake: replay=session_mixed.ndjson\n// fake: delay=0\n// fake: exit=0\nBEGIN {}\n",
        );
        let fixture = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/json/session_mixed.ndjson"
        ))
        .expect("fixture");

        let spool = Spool {
            path: dir.path().join("full.ndjson"),
            cap: SPOOL_CAP,
        };
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let handle = spawn(&argv(&s), tx, FAST, Some(&spool)).expect("spawn");
        collect(&mut rx).await;
        assert_eq!(
            std::fs::read_to_string(&spool.path).expect("spool"),
            fixture,
            "byte for byte"
        );
        assert!(!handle.spool_truncated());

        // A cap smaller than the output: whole lines up to the cap, then truncated.
        let first_two: usize = fixture.lines().take(2).map(|l| l.len() + 1).sum();
        let spool = Spool {
            path: dir.path().join("capped.ndjson"),
            cap: first_two as u64 + 5,
        };
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let handle = spawn(&argv(&s), tx, FAST, Some(&spool)).expect("spawn");
        let (events, _) = collect(&mut rx).await;
        assert_eq!(
            std::fs::read_to_string(&spool.path).expect("spool").len(),
            first_two
        );
        assert!(handle.spool_truncated());
        assert_eq!(events.len(), 5, "the run itself is unaffected by the cap");

        // An unwritable spool path is skipped, the run still works.
        let spool = Spool {
            path: dir.path().join("missing/dir/x.ndjson"),
            cap: SPOOL_CAP,
        };
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let _handle = spawn(&argv(&s), tx, FAST, Some(&spool)).expect("spawn");
        let (_, exit) = collect(&mut rx).await;
        assert_eq!(exit.code, Some(0));
    }

    #[tokio::test]
    async fn spawn_failure_is_an_error() {
        let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
        assert!(spawn(&["/definitely/not/bpftrace".into()], tx.clone(), FAST, None).is_err());
        assert!(spawn(&[], tx, FAST, None).is_err());
    }
}
