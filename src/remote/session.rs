//! One SSH session per operation: `ssh … -- host sh -s`, then the handshakes of
//! docs/design-remote.md, then bpftrace's stdout/stderr as if it ran locally.
//!
//! Rule: nothing is written to the session before the remote reader announced itself
//! (dash reads scripts from a pipe in blocks and would swallow early data).

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

use super::{Dest, REMOTE_SCRIPT};
use crate::bpftrace::runner::{self, Escalation, RunEvent, RunHandle, Spool, Started, StopMode};
use crate::bpftrace::{CaptureError, Captured, signal_group};

/// The runner program (docs/design-remote.md, appendix), sent verbatim on every session.
pub const RUNNER: &str = include_str!("runner.sh");
const PROTOCOL: &str = "bpfdeck-remote 1";
/// Turns the session into a root `sh -s`; prints the "root" marker once it is ready.
const SUDO_NOPASSWD: &str = "exec sudo -n -- sh -c 'echo \"bpfdeck-remote: root\" >&2; exec sh -s'\n";
/// Same with a password on stdin; the "sudo" marker says the shell has read this line.
const SUDO_PASSWORD: &str = "echo 'bpfdeck-remote: sudo' >&2; exec sudo -S -p '' -- sh -c 'echo \"bpfdeck-remote: root\" >&2; exec sh -s'\n";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// A password that never shows up in `Debug` output (logs, `Cmd` dumps, panics).
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: String) -> Self {
        Self(s)
    }
    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// How the session becomes root (D-018).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sudo {
    /// Logged in as root.
    None,
    NoPassword,
    Password(Secret),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    #[error("cannot run ssh: {0}")]
    Spawn(String),
    #[error("sudo needs a password on this host")]
    PasswordRequired,
    #[error("the sudo password was not accepted")]
    WrongPassword,
    #[error("{stage}: {detail}")]
    Handshake { stage: &'static str, detail: String },
    #[error("argument contains a line break, which cannot be sent to a remote host")]
    Newline,
}

/// A connected target: every session multiplexes over its SSH master (ControlPath).
#[derive(Debug, Clone)]
pub struct SshTarget {
    pub dest: Dest,
    /// The `ssh` binary (tests point this at tests/fake_ssh/fake-ssh.sh).
    pub ssh: OsString,
    pub control_path: PathBuf,
    pub sudo: Sudo,
}

/// Where each validation/capture of a target goes.
#[derive(Debug, Clone)]
pub enum Backend {
    Local,
    Ssh(Arc<SshTarget>),
}

impl Backend {
    /// The script path bpftrace sees: the file itself locally, the runner's copy remotely.
    pub fn script_arg<'a>(&self, local: &'a Path) -> &'a Path {
        match self {
            Backend::Local => local,
            Backend::Ssh(_) => Path::new(REMOTE_SCRIPT),
        }
    }

    /// Run `argv` to completion here or on the host; `script` is copied over for a remote run.
    pub async fn capture(
        &self,
        argv: &[OsString],
        script: Option<&Path>,
        timeout: Duration,
    ) -> Result<Captured, CaptureError> {
        match self {
            Backend::Local => crate::bpftrace::capture(argv, timeout).await,
            Backend::Ssh(target) => {
                let content = match script {
                    Some(path) => Some(std::fs::read(path).map_err(CaptureError::Io)?),
                    None => None,
                };
                target.capture(argv, content.as_deref(), timeout).await
            }
        }
    }
}

/// A session whose handshake is done: bpftrace (or the given command) is running.
pub struct Opened {
    pub child: Child,
    pub pgid: u32,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: BufReader<ChildStderr>,
}

/// `bpfdeck-remote 1`, argv count, argv lines, script length, script bytes.
pub fn payload(argv: &[OsString], script: Option<&[u8]>) -> Result<Vec<u8>, SessionError> {
    let mut out = format!("{PROTOCOL}\n{}\n", argv.len()).into_bytes();
    for arg in argv {
        let bytes = arg.as_bytes();
        if bytes.contains(&b'\n') {
            return Err(SessionError::Newline);
        }
        out.extend_from_slice(bytes);
        out.push(b'\n');
    }
    let script = script.unwrap_or_default();
    out.extend_from_slice(format!("{}\n", script.len()).as_bytes());
    out.extend_from_slice(script);
    Ok(out)
}

impl SshTarget {
    /// Options shared by every invocation: reuse the master, never prompt.
    fn common_args(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "-o".into(),
            format!("ControlPath={}", self.control_path.display()).into(),
        ];
        if let Some(port) = self.dest.port {
            args.extend(["-p".into(), port.to_string().into()]);
        }
        if let Some(user) = &self.dest.user {
            args.extend(["-l".into(), user.into()]);
        }
        args
    }

    /// `ssh … -o ControlMaster=yes -o ControlPersist=yes -f -N -- host`. With `batch`,
    /// fails instead of prompting (run it without `batch` in the plain terminal).
    pub fn master_argv(&self, batch: bool) -> Vec<OsString> {
        let mut argv = vec![self.ssh.clone()];
        argv.extend(self.common_args());
        for opt in ["ControlMaster=yes", "ControlPersist=yes", "ConnectTimeout=15"] {
            argv.extend(["-o".into(), opt.into()]);
        }
        if batch {
            argv.extend(["-o".into(), "BatchMode=yes".into()]);
        }
        argv.extend([
            "-f".into(),
            "-N".into(),
            "--".into(),
            self.dest.host.clone().into(),
        ]);
        argv
    }

    /// `ssh -O check|exit -- host` against the master.
    pub fn control_argv(&self, op: &str) -> Vec<OsString> {
        let mut argv = vec![self.ssh.clone()];
        argv.extend(self.common_args());
        argv.extend(["-O".into(), op.into(), "--".into(), self.dest.host.clone().into()]);
        argv
    }

    fn session_command(&self) -> Command {
        let mut cmd = Command::new(&self.ssh);
        cmd.args(self.common_args())
            .args(["-o", "ControlMaster=no", "-o", "BatchMode=yes", "-T", "--"])
            .arg(&self.dest.host)
            .args(["sh", "-s"])
            .process_group(0)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Start `argv` on the host (with `script` as `script.bt` in the runner's temp dir).
    pub async fn open(&self, argv: &[OsString], script: Option<&[u8]>) -> Result<Opened, SessionError> {
        let payload = payload(argv, script)?;
        let mut child = self
            .session_command()
            .spawn()
            .map_err(|e| SessionError::Spawn(e.to_string()))?;
        let pgid = child.id().unwrap_or(0);
        let (Some(mut stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(SessionError::Spawn("ssh pipes missing".into()));
        };
        let mut hs = Handshake {
            stderr: BufReader::new(stderr),
            seen: Vec::new(),
        };
        let result = async {
            match &self.sudo {
                Sudo::None => {}
                Sudo::NoPassword => {
                    hs.send(&mut stdin, SUDO_NOPASSWD.as_bytes(), "sudo").await?;
                    hs.expect("bpfdeck-remote: root", "sudo").await?;
                }
                Sudo::Password(pw) => {
                    hs.send(&mut stdin, SUDO_PASSWORD.as_bytes(), "sudo").await?;
                    hs.expect("bpfdeck-remote: sudo", "sudo").await?;
                    hs.send(&mut stdin, format!("{}\n", pw.expose()).as_bytes(), "sudo")
                        .await?;
                    hs.expect("bpfdeck-remote: root", "sudo").await?;
                }
            }
            hs.send(&mut stdin, RUNNER.as_bytes(), "runner").await?;
            hs.expect("bpfdeck-remote: ready", "runner").await?;
            hs.send(&mut stdin, &payload, "runner").await?;
            hs.expect("bpfdeck-remote: started", "runner").await
        }
        .await;
        match result {
            Ok(()) => Ok(Opened {
                child,
                pgid,
                stdin,
                stdout,
                stderr: hs.stderr,
            }),
            Err(e) => {
                signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
                Err(e)
            }
        }
    }

    /// Run a short command to completion (detection, dry-run, `-l`).
    pub async fn capture(
        &self,
        argv: &[OsString],
        script: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Captured, CaptureError> {
        let work = async {
            let mut opened = self
                .open(argv, script)
                .await
                .map_err(|e| CaptureError::Remote(e.to_string()))?;
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let (r1, r2) = tokio::join!(
                opened.stdout.read_to_end(&mut out),
                opened.stderr.read_to_end(&mut err)
            );
            r1.and(r2).map_err(CaptureError::Io)?;
            let status = opened.child.wait().await.map_err(CaptureError::Io)?;
            drop(opened.stdin);
            Ok(Captured {
                success: status.success(),
                code: status.code(),
                stdout: String::from_utf8_lossy(&out).into_owned(),
                stderr: String::from_utf8_lossy(&err).into_owned(),
            })
        };
        tokio::time::timeout(timeout, work)
            .await
            .unwrap_or(Err(CaptureError::Timeout))
    }

    /// Start a run on the host; events flow exactly as for a local run.
    pub async fn start_run(
        &self,
        argv: &[OsString],
        script: &[u8],
        tx: mpsc::Sender<RunEvent>,
        escalation: Escalation,
        spool: Option<&Spool>,
    ) -> Result<RunHandle, SessionError> {
        let opened = self.open(argv, Some(script)).await?;
        Ok(runner::start(
            Started {
                child: opened.child,
                pgid: opened.pgid,
                stdout: Box::new(opened.stdout),
                stderr: Box::new(opened.stderr),
                stop: StopMode::StdinLine(opened.stdin),
                remote: true,
            },
            tx,
            escalation,
            spool,
        ))
    }
}

/// Reads the session's stderr up to each marker; keeps other lines for error messages.
struct Handshake {
    stderr: BufReader<ChildStderr>,
    seen: Vec<String>,
}

impl Handshake {
    async fn send(
        &mut self,
        stdin: &mut ChildStdin,
        data: &[u8],
        stage: &'static str,
    ) -> Result<(), SessionError> {
        let written = async {
            stdin.write_all(data).await?;
            stdin.flush().await
        }
        .await;
        if written.is_err() {
            // ssh (or the remote shell) is gone: its stderr says why.
            let mut rest = String::new();
            let _ = tokio::time::timeout(Duration::from_secs(2), self.stderr.read_to_string(&mut rest)).await;
            self.seen.extend(rest.lines().map(str::to_string));
            return Err(self.failure(stage));
        }
        Ok(())
    }

    async fn expect(&mut self, marker: &str, stage: &'static str) -> Result<(), SessionError> {
        let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
        loop {
            let mut line = String::new();
            let read = tokio::time::timeout_at(deadline, self.stderr.read_line(&mut line)).await;
            match read {
                Err(_) => {
                    return Err(SessionError::Handshake {
                        stage,
                        detail: format!("no answer within {}s", HANDSHAKE_TIMEOUT.as_secs()),
                    });
                }
                Ok(Ok(0)) | Ok(Err(_)) => return Err(self.failure(stage)),
                Ok(Ok(_)) => {}
            }
            let line = line.trim_end();
            if line == marker {
                return Ok(());
            }
            if line.contains("Sorry, try again") || line.contains("incorrect password") {
                return Err(SessionError::WrongPassword);
            }
            if line.contains("a password is required") {
                return Err(SessionError::PasswordRequired);
            }
            self.seen.push(line.to_string());
        }
    }

    fn failure(&self, stage: &'static str) -> SessionError {
        if self.seen.iter().any(|l| l.contains("a password is required")) {
            return SessionError::PasswordRequired;
        }
        let detail = self
            .seen
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| "the session ended".into());
        SessionError::Handshake { stage, detail }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::Path;

    use super::*;

    pub fn fake_ssh() -> OsString {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fake_ssh/fake-ssh.sh")
            .into()
    }

    pub fn target(host: &str, sudo: Sudo) -> SshTarget {
        SshTarget {
            dest: Dest {
                user: None,
                host: host.into(),
                port: None,
            },
            ssh: fake_ssh(),
            control_path: "/tmp/bpfdeck-test-unused".into(),
            sudo,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use crate::bpftrace::command::{self, RunArgs};
    use crate::bpftrace::json::{HistSeries, OutputMsg};
    use crate::bpftrace::testutil::fake;
    use pretty_assertions::assert_eq;
    use std::path::Path;

    const FAST: Escalation = Escalation {
        after_int: Duration::from_millis(300),
        after_term: Duration::from_millis(300),
    };

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn payload_framing() {
        let p = payload(&os(&["bpftrace", "-f", "two words", "$(x)"]), Some(b"BEGIN {}\n")).expect("payload");
        assert_eq!(
            String::from_utf8(p).expect("utf8"),
            "bpfdeck-remote 1\n4\nbpftrace\n-f\ntwo words\n$(x)\n9\nBEGIN {}\n"
        );
        assert_eq!(payload(&os(&["a\nb"]), None), Err(SessionError::Newline));
        assert_eq!(
            payload(&[], None).expect("empty").len(),
            "bpfdeck-remote 1\n0\n0\n".len()
        );
    }

    #[test]
    fn secrets_do_not_leak_through_debug() {
        let t = target("h", Sudo::Password(Secret::new("hunter2".into())));
        let dump = format!("{t:?}");
        assert!(
            !dump.contains("hunter2") && dump.contains("Secret(***)"),
            "{dump}"
        );
    }

    #[test]
    fn ssh_argv_shapes() {
        let mut t = target("10.0.3.14", Sudo::None);
        t.dest.user = Some("ops".into());
        t.dest.port = Some(2222);
        t.ssh = "ssh".into();
        t.control_path = "/tmp/bpfdeck-501/%C".into();
        let s = |v: Vec<OsString>| {
            v.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(
            s(t.master_argv(true)),
            "ssh -o ControlPath=/tmp/bpfdeck-501/%C -p 2222 -l ops -o ControlMaster=yes -o ControlPersist=yes \
             -o ConnectTimeout=15 -o BatchMode=yes -f -N -- 10.0.3.14"
        );
        assert_eq!(
            s(t.control_argv("exit")),
            "ssh -o ControlPath=/tmp/bpfdeck-501/%C -p 2222 -l ops -O exit -- 10.0.3.14"
        );
    }

    #[tokio::test]
    async fn capture_runs_on_the_host_with_every_sudo_mode() {
        let argv = vec![fake().into_os_string(), "--version".into()];
        for sudo in [Sudo::None, Sudo::NoPassword] {
            let out = target("host", sudo)
                .capture(&argv, None, Duration::from_secs(20))
                .await
                .expect("capture");
            assert!(out.success, "{out:?}");
            assert_eq!(out.stdout.trim(), "bpftrace v0.99.0-fake");
        }
        let pw = Sudo::Password(Secret::new("secret".into()));
        let out = target("pwsudo", pw)
            .capture(&argv, None, Duration::from_secs(20))
            .await
            .expect("capture");
        assert_eq!(out.stdout.trim(), "bpftrace v0.99.0-fake");
    }

    #[tokio::test]
    async fn sudo_failures_are_recognized() {
        let argv = vec![fake().into_os_string(), "--version".into()];
        let err = target("pwsudo", Sudo::NoPassword).open(&argv, None).await.err();
        assert_eq!(err, Some(SessionError::PasswordRequired));
        let wrong = Sudo::Password(Secret::new("nope".into()));
        let err = target("pwsudo", wrong).open(&argv, None).await.err();
        assert_eq!(err, Some(SessionError::WrongPassword));
        let err = target("unreachable-1", Sudo::None).open(&argv, None).await.err();
        match err {
            Some(SessionError::Handshake { detail, .. }) => {
                assert!(detail.contains("Connection refused"), "{detail}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn dry_run_gets_the_script_content() {
        let script = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scripts/missing_probe_demo.bt"),
        )
        .expect("fixture");
        let argv = command::dry_run_argv(&fake(), Path::new("script.bt"), 0);
        let out = target("host", Sudo::None)
            .capture(&argv, Some(&script), Duration::from_secs(20))
            .await
            .expect("capture");
        assert_eq!(out.code, Some(1));
        assert!(
            out.stderr.contains("this_function_does_not_exist_bpfdeck"),
            "{}",
            out.stderr
        );
    }

    #[tokio::test]
    async fn remote_run_streams_and_stops_with_the_exit_dump() {
        let script = b"// fake: replay=session_mixed.ndjson\n// fake: delay=0\nBEGIN {}\n";
        let args = RunArgs {
            script: Path::new("script.bt"),
            positional: &["two words".to_string()],
            named: &[],
            allow_unsafe: false,
        };
        let argv = command::run_argv(&fake(), &args).expect("argv");
        let (tx, mut rx) = mpsc::channel(runner::CHANNEL_CAPACITY);
        let mut handle = target("host", Sudo::NoPassword)
            .start_run(&argv, script, tx, FAST, None)
            .await
            .expect("start");
        let mut events = Vec::new();
        while events.len() < 5 {
            events.push(
                tokio::time::timeout(Duration::from_secs(10), rx.recv())
                    .await
                    .expect("event")
                    .expect("open"),
            );
        }
        assert_eq!(events[0], RunEvent::Output(OutputMsg::AttachedProbes(3)));
        handle.stop();
        let mut rest = Vec::new();
        let exit = loop {
            match tokio::time::timeout(Duration::from_secs(15), rx.recv())
                .await
                .expect("exit")
                .expect("open")
            {
                RunEvent::Exited(exit) => break exit,
                other => rest.push(other),
            }
        };
        assert!(
            rest.iter().any(|e| matches!(e, RunEvent::Output(OutputMsg::Hist { name, series: HistSeries::Single(_) }) if name == "@final")),
            "exit-time dump over ssh: {rest:?}"
        );
        assert_eq!(exit.code, Some(0), "{exit:?}");
    }

    #[tokio::test]
    async fn remote_stop_escalates_and_reports_signals() {
        // A "bpftrace" that ignores SIGINT: the runner escalates to SIGTERM on the host.
        let argv = os(&["/bin/sh", "-c", "trap '' INT; while :; do sleep 0.1; done"]);
        let (tx, mut rx) = mpsc::channel(runner::CHANNEL_CAPACITY);
        let mut handle = target("host", Sudo::None)
            .start_run(&argv, b"", tx, Escalation::default(), None)
            .await
            .expect("start");
        // "started" comes before the command had a chance to install its trap.
        tokio::time::sleep(Duration::from_millis(300)).await;
        handle.stop();
        let exit = loop {
            if let RunEvent::Exited(exit) = tokio::time::timeout(Duration::from_secs(20), rx.recv())
                .await
                .expect("exit")
                .expect("open")
            {
                break exit;
            }
        };
        assert_eq!((exit.code, exit.signal), (None, Some(15)), "{exit:?}");
        assert_eq!(exit.forced, Some(nix::sys::signal::Signal::SIGTERM));
    }
}
