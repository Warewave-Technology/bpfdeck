//! The connect checks (docs/design-remote.md, "Connecting"): the SSH master connection,
//! host facts, root, bpftrace, kernel. Each check is reported as soon as it is done; the
//! first hard failure ends the attempt.

use std::ffi::OsString;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::process::Command;
use tokio::sync::mpsc;

use super::facts::{FACTS_SCRIPT, Facts, RemoteInfo};
use super::session::{Backend, Secret, SessionError, SshTarget, Sudo};
use crate::bpftrace::{self, BpftraceInfo};
use crate::sys::SystemInfo;

const MASTER_TIMEOUT: Duration = Duration::from_secs(30);
const FACTS_TIMEOUT: Duration = Duration::from_secs(20);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

/// How the connect dialog asks to become root (D-018).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SudoChoice {
    /// Root login if that is who we are, else `sudo -n`.
    Auto,
    /// Must be logged in as root.
    Root,
    Password(Secret),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    /// Does not block the connection.
    Warn,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub status: CheckStatus,
    pub text: String,
}

impl Check {
    fn ok(text: String) -> Self {
        Self {
            status: CheckStatus::Ok,
            text,
        }
    }
    fn warn(text: String) -> Self {
        Self {
            status: CheckStatus::Warn,
            text,
        }
    }
    fn fail(text: String) -> Self {
        Self {
            status: CheckStatus::Fail,
            text,
        }
    }
}

/// A host that passed the checks.
#[derive(Debug)]
pub struct Connected {
    /// With the sudo mode that worked.
    pub target: Arc<SshTarget>,
    pub info: RemoteInfo,
    pub system: SystemInfo,
    pub bpftrace: BpftraceInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// SSH wants a human (password, passphrase, 2FA, unknown host key): retry in the
    /// terminal. The text is ssh's own message.
    NeedsAuth(String),
    /// Reported as the last (failed) check.
    Failed,
}

/// `/tmp/bpfdeck-<uid>`: private (0700, ours, not a symlink) and short, because unix
/// socket paths are limited to ~104 bytes.
pub fn control_dir() -> Result<PathBuf, String> {
    let uid = nix::unistd::getuid().as_raw();
    let dir = PathBuf::from(format!("/tmp/bpfdeck-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "{} must be a directory owned by you with mode 0700",
            dir.display()
        ));
    }
    Ok(dir)
}

/// ssh's message log for one connect attempt (`-E`), inside the control dir.
pub fn master_log(dir: &Path, attempt: u64) -> PathBuf {
    dir.join(format!("connect-{attempt}.log"))
}

/// Read and remove the master's log; its last lines are ssh's error message.
fn take_log(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let _ = std::fs::remove_file(log);
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(3)..].join(" · ")
}

/// Open the master connection without prompting. `Err`: ssh's message.
async fn open_master(target: &SshTarget, log: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(log);
    let argv = target.master_argv(true, log);
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match tokio::time::timeout(MASTER_TIMEOUT, status).await {
        Ok(Ok(s)) if s.success() => {
            let _ = std::fs::remove_file(log);
            Ok(())
        }
        Ok(Ok(s)) => {
            let message = take_log(log);
            Err(if message.is_empty() {
                format!("ssh exited with {s}")
            } else {
                message
            })
        }
        Ok(Err(e)) => Err(format!("cannot run {}: {e}", target.ssh.to_string_lossy())),
        Err(_) => Err(format!("no connection within {}s", MASTER_TIMEOUT.as_secs())),
    }
}

/// Whether ssh failed for something a person can answer in the terminal.
fn needs_human(message: &str) -> bool {
    if message.contains("IDENTIFICATION HAS CHANGED") {
        return false;
    }
    [
        "Permission denied",
        "Host key verification failed",
        "passphrase",
        "keyboard-interactive",
        "Too many authentication failures",
    ]
    .iter()
    .any(|m| message.contains(m))
}

/// `ssh -O exit`: ends the master (and so every session over it).
pub async fn close_master(target: &SshTarget) {
    let _ = control(target, "exit").await;
}

/// `ssh -O check`: whether the master is still alive.
pub async fn master_alive(target: &SshTarget) -> bool {
    control(target, "check").await
}

async fn control(target: &SshTarget, op: &str) -> bool {
    let argv = target.control_argv(op);
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    matches!(tokio::time::timeout(CONTROL_TIMEOUT, status).await, Ok(Ok(s)) if s.success())
}

async fn facts(target: &SshTarget) -> Result<Facts, SessionError> {
    let argv: Vec<OsString> = vec!["sh".into(), super::REMOTE_SCRIPT.into()];
    let out = target
        .capture(&argv, Some(FACTS_SCRIPT.as_bytes()), FACTS_TIMEOUT)
        .await
        .map_err(|e| match e {
            crate::bpftrace::CaptureError::Remote(detail) => SessionError::Handshake {
                stage: "session",
                detail,
            },
            other => SessionError::Handshake {
                stage: "session",
                detail: format!("{other:?}"),
            },
        })?;
    if !out.success {
        return Err(SessionError::Handshake {
            stage: "host facts",
            detail: out.combined(),
        });
    }
    Ok(Facts::parse(&out.stdout))
}

async fn send(report: &mpsc::Sender<Check>, check: Check) {
    let _ = report.send(check).await;
}

async fn fail(report: &mpsc::Sender<Check>, text: String) -> Failure {
    send(report, Check::fail(text)).await;
    Failure::Failed
}

/// Run the checks against `base` (its `sudo` is ignored). `master_open`: the master was
/// already opened in the terminal. Each check goes to `report` as it completes.
pub async fn connect(
    base: SshTarget,
    choice: SudoChoice,
    bpftrace_path: Option<String>,
    log: &Path,
    master_open: bool,
    report: &mpsc::Sender<Check>,
) -> Result<Connected, Failure> {
    let started = Instant::now();
    if !master_open && let Err(message) = open_master(&base, log).await {
        if needs_human(&message) {
            return Err(Failure::NeedsAuth(message));
        }
        send(report, Check::fail(format!("ssh: {message}"))).await;
        return Err(Failure::Failed);
    }
    let login = SshTarget {
        sudo: Sudo::None,
        ..base
    };
    let user_facts = match facts(&login).await {
        Ok(f) => f,
        Err(e) => return Err(fail(report, format!("ssh: {e}")).await),
    };
    send(
        report,
        Check::ok(format!(
            "ssh: connected as {} · {} ms",
            user_facts.user,
            started.elapsed().as_millis()
        )),
    )
    .await;
    send(
        report,
        Check::ok(format!(
            "host: {} · {} · {}",
            if user_facts.os.is_empty() {
                "unknown OS"
            } else {
                &user_facts.os
            },
            user_facts.arch,
            user_facts.kernel
        )),
    )
    .await;
    let mut tools = vec!["sh", "mktemp", "head"];
    if user_facts.has("setsid") {
        tools.push("setsid");
        send(report, Check::ok(format!("shell: {}", tools.join(", ")))).await;
    } else {
        send(
            report,
            Check::warn(format!(
                "shell: {}; no setsid, so processes started by system() may outlive a stop",
                tools.join(", ")
            )),
        )
        .await;
    }

    // Become root.
    let (sudo, privilege) = match (&choice, user_facts.is_root()) {
        (_, true) => (Sudo::None, "root"),
        (SudoChoice::Root, false) => {
            return Err(fail(
                report,
                format!(
                    "root: logged in as {}, not root; pick automatic sudo or sudo with password",
                    user_facts.user
                ),
            )
            .await);
        }
        (SudoChoice::Auto, false) => (Sudo::NoPassword, "root via sudo"),
        (SudoChoice::Password(pw), false) => (Sudo::Password(pw.clone()), "root via sudo (password)"),
    };
    let target = SshTarget { sudo, ..login };
    let root_facts = if target.sudo == Sudo::None {
        user_facts.clone()
    } else {
        match facts(&target).await {
            Ok(f) if f.is_root() => f,
            Ok(f) => return Err(fail(report, format!("root: sudo ran as uid {:?}, not root", f.uid)).await),
            Err(SessionError::PasswordRequired) => {
                return Err(fail(
                    report,
                    "root: sudo needs a password here: pick 'sudo with password' or log in as root".into(),
                )
                .await);
            }
            Err(SessionError::WrongPassword) => {
                return Err(fail(report, "root: the sudo password was not accepted".into()).await);
            }
            Err(e) => return Err(fail(report, format!("root: sudo failed: {e}")).await),
        }
    };
    send(
        report,
        Check::ok(match &target.sudo {
            Sudo::None => "root: logged in as root".to_string(),
            Sudo::NoPassword => "root: sudo -n works".to_string(),
            Sudo::Password(_) => "root: sudo with password works".to_string(),
        }),
    )
    .await;

    // bpftrace, as root.
    let path = bpftrace_path
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| root_facts.bpftrace.clone());
    if path.is_empty() {
        return Err(fail(
            report,
            "bpftrace: not found in root's PATH; enter its path in the bpftrace field".into(),
        )
        .await);
    }
    let target = Arc::new(target);
    let backend = Backend::Ssh(target.clone());
    let info = match bpftrace::detect_on(&backend, Path::new(&path)).await {
        Ok(info) => info,
        Err(e) => return Err(fail(report, format!("bpftrace: {e}")).await),
    };
    let version = info
        .version
        .map_or_else(|| info.version_raw.clone(), |v| v.to_string());
    if info.supports_dry_run {
        send(
            report,
            Check::ok(format!("bpftrace {version} at {path} · --dry-run supported")),
        )
        .await;
    } else {
        send(
            report,
            Check::warn(format!(
                "bpftrace {version} at {path} · no --dry-run: validation only looks up probes"
            )),
        )
        .await;
    }

    let system = root_facts.system_info();
    let btf = if root_facts.btf { "BTF present" } else { "no BTF" };
    let lockdown = format!("{:?}", system.lockdown).to_lowercase();
    if system.lockdown.blocks_bpftrace() {
        send(
            report,
            Check::warn(format!(
                "kernel: lockdown {lockdown}: bpftrace cannot load programs · {btf}"
            )),
        )
        .await;
    } else {
        send(report, Check::ok(format!("kernel: lockdown {lockdown} · {btf}"))).await;
    }
    Ok(Connected {
        info: RemoteInfo {
            user: user_facts.user,
            os: root_facts.os,
            arch: root_facts.arch,
            privilege: privilege.to_string(),
            btf: root_facts.btf,
        },
        target,
        system,
        bpftrace: info,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpftrace::testutil::fake;
    use crate::remote::session::testutil;
    use crate::sys::Privilege;

    struct Attempt {
        result: Result<Connected, Failure>,
        checks: Vec<Check>,
    }

    async fn attempt(host: &str, choice: SudoChoice, bpftrace: Option<String>, master_open: bool) -> Attempt {
        let dir = tempfile::tempdir().expect("tempdir");
        let (tx, mut rx) = mpsc::channel(64);
        let result = connect(
            testutil::target(host, Sudo::None),
            choice,
            bpftrace,
            &master_log(dir.path(), 1),
            master_open,
            &tx,
        )
        .await;
        drop(tx);
        let mut checks = Vec::new();
        while let Some(c) = rx.recv().await {
            checks.push(c);
        }
        Attempt { result, checks }
    }

    fn connected(a: Attempt) -> Connected {
        match a.result {
            Ok(c) => c,
            Err(e) => panic!("{e:?}\n{}", texts(&a.checks)),
        }
    }

    fn texts(checks: &[Check]) -> String {
        checks
            .iter()
            .map(|c| format!("{:?} {}", c.status, c.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn sudo_host_passes_every_check() {
        let a = attempt("db-02", SudoChoice::Auto, None, false).await;
        let all = texts(&a.checks);
        let connected = a.result.expect(&all);
        assert_eq!(connected.info.privilege, "root via sudo");
        assert_eq!(connected.target.sudo, Sudo::NoPassword);
        assert_eq!(connected.system.privilege, Privilege::Root);
        assert!(connected.bpftrace.supports_dry_run);
        assert!(all.starts_with("Ok ssh: connected as "), "{all}");
        for want in [
            "Ok root: sudo -n works",
            "Ok bpftrace v0.99.0 at ",
            "kernel: lockdown",
        ] {
            assert!(all.contains(want), "{want} in\n{all}");
        }
        assert!(a.checks.iter().all(|c| c.status != CheckStatus::Fail), "{all}");
    }

    #[tokio::test]
    async fn root_login_and_root_choice() {
        let a = attempt("rootlogin-1", SudoChoice::Root, None, false).await;
        let connected = connected(a);
        assert_eq!(
            (connected.target.sudo.clone(), connected.info.privilege.as_str()),
            (Sudo::None, "root")
        );

        let a = attempt("plain", SudoChoice::Root, None, false).await;
        assert_eq!(a.result.err(), Some(Failure::Failed));
        let last = a.checks.last().expect("a check");
        assert_eq!(last.status, CheckStatus::Fail);
        assert!(last.text.contains("not root"), "{}", last.text);
    }

    #[tokio::test]
    async fn sudo_passwords() {
        let a = attempt("pwsudo", SudoChoice::Auto, None, false).await;
        assert!(a.result.is_err());
        assert!(
            texts(&a.checks).contains("sudo needs a password"),
            "{}",
            texts(&a.checks)
        );

        let wrong = SudoChoice::Password(Secret::new("nope".into()));
        let a = attempt("pwsudo", wrong, None, false).await;
        assert!(
            texts(&a.checks).contains("password was not accepted"),
            "{}",
            texts(&a.checks)
        );

        let right = SudoChoice::Password(Secret::new("secret".into()));
        let a = attempt("pwsudo", right, None, false).await;
        let connected = connected(a);
        assert_eq!(connected.info.privilege, "root via sudo (password)");
    }

    #[tokio::test]
    async fn ssh_failures() {
        let a = attempt("unreachable", SudoChoice::Auto, None, false).await;
        assert_eq!(a.result.err(), Some(Failure::Failed));
        assert!(
            texts(&a.checks)
                .contains("Fail ssh: ssh: connect to host unreachable port 22: Connection refused"),
            "{}",
            texts(&a.checks)
        );

        let a = attempt("needs-auth", SudoChoice::Auto, None, false).await;
        match a.result {
            Err(Failure::NeedsAuth(m)) => assert!(m.contains("Permission denied"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(a.checks.is_empty());
        // After authenticating in the terminal, the master exists.
        let a = attempt("needs-auth", SudoChoice::Auto, None, true).await;
        assert!(a.result.is_ok(), "{}", texts(&a.checks));
    }

    #[tokio::test]
    async fn bpftrace_location() {
        let a = attempt("nobpftrace", SudoChoice::Auto, None, false).await;
        assert!(
            texts(&a.checks).contains("Fail bpftrace: not found in root's PATH"),
            "{}",
            texts(&a.checks)
        );
        let path = fake().to_string_lossy().into_owned();
        let a = attempt("nobpftrace", SudoChoice::Auto, Some(path.clone()), false).await;
        assert_eq!(connected(a).bpftrace.path, Path::new(&path));
    }

    #[test]
    fn classifies_ssh_messages() {
        assert!(needs_human("ops@h: Permission denied (publickey)."));
        assert!(needs_human("Host key verification failed."));
        assert!(!needs_human(
            "@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@ · Host key verification failed."
        ));
        assert!(!needs_human("ssh: connect to host h port 22: Connection refused"));
    }

    #[test]
    fn control_dir_is_private() {
        let dir = control_dir().expect("control dir");
        let meta = std::fs::symlink_metadata(&dir).expect("meta");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
        assert!(
            dir.as_os_str().len() < 40,
            "short for socket paths: {}",
            dir.display()
        );
    }
}
