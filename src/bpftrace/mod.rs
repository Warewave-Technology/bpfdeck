//! Everything that talks to bpftrace: output parsing, argv building, capability detection,
//! running and validation.

pub mod command;
pub mod json;
pub mod runner;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use regex::Regex;
use tokio::process::Command;

const DETECT_TIMEOUT: Duration = Duration::from_secs(10);

/// What the installed bpftrace can do, detected once at startup (D-006).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BpftraceInfo {
    pub path: PathBuf,
    /// Parsed `--version`, if recognizable.
    pub version: Option<Version>,
    /// `--version` output as printed (part of the validation cache key).
    pub version_raw: String,
    /// `--dry-run` is listed in `--help`.
    pub supports_dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error("cannot run {path}: {source}")]
    Spawn { path: PathBuf, source: io::Error },
    #[error("{path} --version timed out")]
    Timeout { path: PathBuf },
    #[error("{path} --version failed: {output}")]
    Failed { path: PathBuf, output: String },
}

/// Run `--version` and `--help`. `--help` goes to stderr and exits non-zero on older
/// versions, so both streams are read and its status is ignored.
pub async fn detect(path: &Path) -> Result<BpftraceInfo, DetectError> {
    let to_err = |e| match e {
        CaptureError::Spawn(source) | CaptureError::Io(source) => DetectError::Spawn {
            path: path.to_path_buf(),
            source,
        },
        CaptureError::Timeout => DetectError::Timeout {
            path: path.to_path_buf(),
        },
    };
    let version = capture(&command::version_argv(path), DETECT_TIMEOUT)
        .await
        .map_err(to_err)?;
    if !version.success {
        return Err(DetectError::Failed {
            path: path.to_path_buf(),
            output: version.combined(),
        });
    }
    let help = capture(&command::help_argv(path), DETECT_TIMEOUT)
        .await
        .map_err(to_err)?;
    let version_raw = version.stdout.trim().to_string();
    Ok(BpftraceInfo {
        path: path.to_path_buf(),
        version: parse_version(&version_raw),
        version_raw,
        supports_dry_run: help_mentions_dry_run(&help.combined()),
    })
}

/// `bpftrace v0.21.2`, `bpftrace v0.9.4-12-gabcdef`, `bpftrace 0.20.0` …
pub fn parse_version(s: &str) -> Option<Version> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\bv?(\d+)\.(\d+)(?:\.(\d+))?").expect("static regex"));
    let c = RE.captures(s)?;
    let num = |i| c.get(i).map_or(Some(0), |m| m.as_str().parse().ok());
    Some(Version {
        major: num(1)?,
        minor: num(2)?,
        patch: num(3)?,
    })
}

fn help_mentions_dry_run(help: &str) -> bool {
    help.contains("--dry-run")
}

/// Output of a short-lived bpftrace invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Captured {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    pub fn combined(&self) -> String {
        let (out, err) = (self.stdout.trim(), self.stderr.trim());
        match (out.is_empty(), err.is_empty()) {
            (_, true) => out.to_string(),
            (true, false) => err.to_string(),
            (false, false) => format!("{err}\n{out}"),
        }
    }
}

#[derive(Debug)]
pub(crate) enum CaptureError {
    Spawn(io::Error),
    Io(io::Error),
    Timeout,
}

/// Run `argv` (no shell) in its own process group and collect its output. On timeout
/// the whole group is killed, so nothing it started outlives the call.
pub(crate) async fn capture(argv: &[OsString], timeout: Duration) -> Result<Captured, CaptureError> {
    let Some((program, args)) = argv.split_first() else {
        return Err(CaptureError::Spawn(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty argv",
        )));
    };
    let child = Command::new(program)
        .args(args)
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(CaptureError::Spawn)?;
    let pgid = child.id();
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => Ok(Captured {
            success: out.status.success(),
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
        Ok(Err(e)) => Err(CaptureError::Io(e)),
        Err(_) => {
            if let Some(pgid) = pgid {
                signal_group(pgid, Signal::SIGKILL);
            }
            Err(CaptureError::Timeout)
        }
    }
}

/// Signal a process group; "no such process" (already gone) is not an error.
pub(crate) fn signal_group(pgid: u32, signal: Signal) {
    if let Ok(raw) = i32::try_from(pgid)
        && raw > 0
    {
        let _ = killpg(Pid::from_raw(raw), signal);
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};

    pub fn fake() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_bpftrace/fake-bpftrace.sh")
    }

    pub fn fake_old() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_bpftrace/fake-bpftrace-old.sh")
    }

    /// A temp `.bt` file whose content (and `// fake:` directives) drive the fake.
    pub fn script(dir: &tempfile::TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).expect("write script");
        path
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn versions() {
        let v = |major, minor, patch| Some(Version { major, minor, patch });
        assert_eq!(parse_version("bpftrace v0.21.2"), v(0, 21, 2));
        assert_eq!(parse_version("bpftrace v0.9.4-12-gabcdef"), v(0, 9, 4));
        assert_eq!(parse_version("bpftrace 0.20"), v(0, 20, 0));
        assert_eq!(parse_version("bpftrace v0.99.0-fake"), v(0, 99, 0));
        assert_eq!(parse_version("bpftrace"), None);
        assert!(v(0, 21, 0) > v(0, 9, 4));
        assert_eq!(
            Version {
                major: 0,
                minor: 21,
                patch: 2
            }
            .to_string(),
            "v0.21.2"
        );
    }

    #[tokio::test]
    async fn detect_new_and_old() {
        let info = detect(&fake()).await.expect("detect");
        assert_eq!(info.version_raw, "bpftrace v0.99.0-fake");
        assert_eq!(info.version, parse_version("0.99.0"));
        assert!(info.supports_dry_run);

        let old = detect(&fake_old()).await.expect("detect old");
        assert_eq!(old.version, parse_version("0.9.4"));
        assert!(
            !old.supports_dry_run,
            "help on stderr with exit 1 must still be read"
        );
    }

    #[tokio::test]
    async fn detect_missing_binary() {
        let err = detect(Path::new("/definitely/not/bpftrace"))
            .await
            .expect_err("missing");
        assert!(matches!(err, DetectError::Spawn { .. }), "{err}");
    }

    #[tokio::test]
    async fn capture_timeout_kills_group() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Run mode never exits by itself and leaves a background child in its group.
        let script = script(&dir, "t.bt", "// fake: child=1\nBEGIN {}\n");
        let argv: Vec<OsString> = vec![fake().into(), "--".into(), script.into()];
        let started = std::time::Instant::now();
        let err = capture(&argv, Duration::from_millis(300))
            .await
            .expect_err("timeout");
        assert!(matches!(err, CaptureError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
