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
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
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

    let spool_truncated = Arc::new(AtomicBool::new(false));
    let spool = spool.and_then(|s| {
        let file = File::create(&s.path).ok()?;
        Some(SpoolWriter {
            out: BufWriter::new(file),
            written: 0,
            cap: s.cap,
            truncated: spool_truncated.clone(),
        })
    });
    let readers = [
        child.stdout.take().map(|out| {
            tokio::spawn(read_lines(out, tx.clone(), spool, |line| {
                json::parse_line(&line)
                    .into_iter()
                    .map(RunEvent::Output)
                    .collect()
            }))
        }),
        child.stderr.take().map(|err| {
            tokio::spawn(read_lines(err, tx.clone(), None, |line| {
                if line.trim().is_empty() {
                    Vec::new()
                } else {
                    vec![RunEvent::Stderr(line)]
                }
            }))
        }),
    ];

    let (stop_tx, stop_rx) = oneshot::channel();
    let finished = Arc::new(AtomicBool::new(false));
    tokio::spawn(supervise(
        child,
        pgid,
        stop_rx,
        escalation,
        readers.into_iter().flatten().collect(),
        tx,
        finished.clone(),
    ));
    Ok(RunHandle {
        pgid,
        stop: Some(stop_tx),
        finished,
        spool_truncated,
    })
}

async fn supervise(
    mut child: Child,
    pgid: u32,
    mut stop_rx: oneshot::Receiver<()>,
    escalation: Escalation,
    readers: Vec<JoinHandle<()>>,
    tx: mpsc::Sender<RunEvent>,
    finished: Arc<AtomicBool>,
) {
    let mut forced = None;
    let status = tokio::select! {
        status = child.wait() => status,
        stop = &mut stop_rx => {
            if stop.is_ok() {
                stop_gracefully(&mut child, pgid, escalation, &mut forced).await
            } else {
                // Handle dropped: its Drop already sent SIGKILL.
                child.wait().await
            }
        }
    };
    finished.store(true, Ordering::Release);
    // bpftrace is gone; take down anything it left in its group (e.g. `-c` commands,
    // `system()` children) so they don't hold the pipes open.
    signal_group(pgid, Signal::SIGKILL);

    for mut reader in readers {
        if tokio::time::timeout(DRAIN_TIMEOUT, &mut reader).await.is_err() {
            reader.abort();
        }
    }
    let _ = tx.send(RunEvent::Exited(exit_of(status, forced))).await;
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
