//! `git` subprocess for git sources (D-004, D-012): shallow fetch into a cache dir.
//!
//! Clone and update are the same operation: `init` (first time only), `fetch --depth 1`
//! of the ref, `reset --hard FETCH_HEAD`. Hooks, submodules and prompts are disabled on
//! every invocation.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use super::SourceError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSpec {
    /// URL without the `#ref` suffix.
    pub url: String,
    /// Branch, tag or commit; `None` = remote HEAD.
    pub reference: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// First checkout into an empty cache dir.
    Cloned,
    /// Existing cache dir fetched and reset to the ref.
    Updated,
    /// Update failed; the previous checkout is kept and used.
    Stale { reason: String },
}

/// `cache_root/repos/<sha256(url)[..16]>`.
pub fn repo_dir(cache_root: &Path, url: &str) -> PathBuf {
    let digest = Sha256::digest(url.as_bytes());
    cache_root.join("repos").join(hex::encode(&digest[..8]))
}

/// Config forced on every git invocation. Hooks could run code from the user's config
/// (and would, for hook types like `reference-transaction`); submodules and fsmonitor
/// likewise; `ext::` URLs execute commands by design.
const SAFE_CONFIG: &[&str] = &[
    "core.hooksPath=/dev/null",
    "submodule.recurse=false",
    "core.fsmonitor=false",
    "protocol.ext.allow=never",
];

pub struct Git {
    program: OsString,
    envs: Vec<(OsString, OsString)>,
}

impl Default for Git {
    fn default() -> Self {
        Self {
            program: "git".into(),
            envs: Vec::new(),
        }
    }
}

impl Git {
    /// Bring `dir` to the state of `spec`. On a fresh dir any failure is an error (and the
    /// partial dir is removed); on an existing checkout it degrades to [`SyncOutcome::Stale`].
    pub fn sync(&self, spec: &GitSpec, dir: &Path) -> Result<SyncOutcome, SourceError> {
        if let Some(r) = &spec.reference {
            validate_ref(r)?;
        }
        let io = |source| SourceError::Io {
            path: dir.to_path_buf(),
            source,
        };
        if dir.join(".git").is_dir() {
            return match self
                .run(dir, spec, &["remote", "set-url", "origin", &spec.url])
                .and_then(|()| self.fetch_and_reset(spec, dir))
            {
                Ok(()) => Ok(SyncOutcome::Updated),
                Err(e) if self.has_checkout(dir) => Ok(SyncOutcome::Stale {
                    reason: e.to_string(),
                }),
                Err(e) => Err(e),
            };
        }

        // Leftover of an interrupted first clone: start over.
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(io)?;
        }
        fs::create_dir_all(dir).map_err(io)?;
        let result = self
            .run(dir, spec, &["init", "-q"])
            .and_then(|()| self.run(dir, spec, &["remote", "add", "origin", &spec.url]))
            .and_then(|()| self.fetch_and_reset(spec, dir));
        match result {
            Ok(()) => Ok(SyncOutcome::Cloned),
            Err(e) => {
                let _ = fs::remove_dir_all(dir);
                Err(e)
            }
        }
    }

    fn fetch_and_reset(&self, spec: &GitSpec, dir: &Path) -> Result<(), SourceError> {
        let target = spec.reference.as_deref().unwrap_or("HEAD");
        let shallow = [
            "fetch",
            "-q",
            "--depth",
            "1",
            "--no-tags",
            "--no-recurse-submodules",
            "--",
            "origin",
            target,
        ];
        match self.run(dir, spec, &shallow) {
            Ok(()) => self.run(dir, spec, &["reset", "-q", "--hard", "FETCH_HEAD"]),
            // Abbreviated hashes (and servers that refuse unadvertised commits) cannot be
            // fetched directly: fetch everything, then resolve the commit locally.
            Err(_) if is_commit_hash(target) => {
                let mut full = vec!["fetch", "-q", "--tags", "--no-recurse-submodules"];
                if dir.join(".git/shallow").exists() {
                    full.push("--unshallow");
                }
                full.extend(["--", "origin", "+refs/heads/*:refs/remotes/origin/*"]);
                self.run(dir, spec, &full)?;
                let commit = format!("{target}^{{commit}}");
                let sha = self.output(
                    dir,
                    spec,
                    &["rev-parse", "--verify", "--quiet", "--end-of-options", &commit],
                )?;
                self.run(dir, spec, &["reset", "-q", "--hard", sha.trim()])
            }
            Err(e) => Err(e),
        }
    }

    fn has_checkout(&self, dir: &Path) -> bool {
        self.command(dir)
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    fn command(&self, dir: &Path) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.arg("-C").arg(dir);
        for kv in SAFE_CONFIG {
            cmd.args(["-c", kv]);
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .envs(self.envs.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, dir: &Path, spec: &GitSpec, args: &[&str]) -> Result<(), SourceError> {
        self.output(dir, spec, args).map(drop)
    }

    fn output(&self, dir: &Path, spec: &GitSpec, args: &[&str]) -> Result<String, SourceError> {
        let out = self
            .command(dir)
            .args(args)
            .output()
            .map_err(SourceError::GitMissing)?;
        if out.status.success() {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(SourceError::Git {
            step: args.first().copied().unwrap_or_default().to_string(),
            url: spec.url.clone(),
            stderr: if stderr.is_empty() {
                format!("exit status {}", out.status)
            } else {
                stderr
            },
        })
    }
}

/// Refs come from user input and end up in git's argv: anything that could be read as an
/// option (`#--upload-pack=…`) or is not a plausible ref name is rejected up front.
fn validate_ref(r: &str) -> Result<(), SourceError> {
    let bad = r.is_empty() || r.starts_with('-') || r.chars().any(|c| c.is_whitespace() || c.is_control());
    if bad {
        return Err(SourceError::InvalidRef(r.to_string()));
    }
    Ok(())
}

fn is_commit_hash(r: &str) -> bool {
    (7..=40).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn repo_dir_is_stable_and_short() {
        let a = repo_dir(Path::new("/c"), "https://github.com/bpftrace/bpftrace");
        let b = repo_dir(Path::new("/c"), "https://github.com/bpftrace/bpftrace");
        let c = repo_dir(Path::new("/c"), "https://github.com/bpftrace/bpftrace.git");
        assert_eq!(a, b);
        assert_ne!(a, c);
        let name = a.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        assert_eq!(name.len(), 16);
        assert!(name.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(a.starts_with("/c/repos"));
    }

    #[test]
    fn refs() {
        for ok in ["main", "v0.27.0", "feature/x", "abc1234"] {
            assert!(validate_ref(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "--upload-pack=touch /tmp/pwned", "a b", "a\nb"] {
            assert!(validate_ref(bad).is_err(), "{bad:?}");
        }
        assert!(is_commit_hash("abc1234"));
        assert!(is_commit_hash(&"a".repeat(40)));
        assert!(!is_commit_hash("abc123"));
        assert!(!is_commit_hash("main"));
        assert!(!is_commit_hash("v1.0.0"));
    }

    /// Tests below drive the real `git` binary against local `file://` upstreams. Git is
    /// isolated from the user's config via GIT_CONFIG_GLOBAL/NOSYSTEM. Skipped (with a
    /// note) when git is not installed.
    struct Fixture {
        _tmp: tempfile::TempDir,
        upstream: PathBuf,
        cache: PathBuf,
        global_config: PathBuf,
    }

    impl Fixture {
        fn new() -> Option<Self> {
            if Command::new("git").arg("--version").output().is_err() {
                eprintln!("git not installed; skipping git sync test");
                return None;
            }
            let tmp = tempfile::tempdir().expect("tempdir");
            let root = fs::canonicalize(tmp.path()).expect("canonicalize");
            let fx = Self {
                upstream: root.join("upstream"),
                cache: root.join("cache"),
                global_config: root.join("gitconfig"),
                _tmp: tmp,
            };
            fs::write(&fx.global_config, "").expect("gitconfig");
            fs::create_dir_all(&fx.upstream).expect("mkdir");
            fx.git_upstream(&["-c", "init.defaultBranch=main", "init", "-q"]);
            fx.commit("a.bt", "BEGIN {}");
            Some(fx)
        }

        fn env(&self) -> Vec<(OsString, OsString)> {
            vec![
                ("GIT_CONFIG_GLOBAL".into(), self.global_config.clone().into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ]
        }

        fn git(&self) -> Git {
            Git {
                program: "git".into(),
                envs: self.env(),
            }
        }

        fn git_upstream(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.upstream)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(["-c", "core.hooksPath=/dev/null"])
                .args(args)
                .envs(self.env())
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn commit(&self, file: &str, content: &str) -> String {
            fs::write(self.upstream.join(file), content).expect("write");
            self.git_upstream(&["add", "-A"]);
            self.git_upstream(&["commit", "-q", "-m", file]);
            self.git_upstream(&["rev-parse", "HEAD"])
        }

        fn spec(&self, reference: Option<&str>) -> GitSpec {
            GitSpec {
                url: format!("file://{}", self.upstream.display()),
                reference: reference.map(Into::into),
            }
        }

        fn dir(&self) -> PathBuf {
            repo_dir(&self.cache, &self.spec(None).url)
        }

        fn sync(&self, reference: Option<&str>) -> Result<SyncOutcome, SourceError> {
            self.git().sync(&self.spec(reference), &self.dir())
        }
    }

    #[test]
    fn clone_then_update() {
        let Some(fx) = Fixture::new() else { return };
        assert_eq!(fx.sync(None).expect("clone"), SyncOutcome::Cloned);
        assert!(fx.dir().join("a.bt").is_file());
        assert!(fx.dir().join(".git/shallow").is_file(), "clone must be shallow");

        fx.commit("b.bt", "END {}");
        assert_eq!(fx.sync(None).expect("update"), SyncOutcome::Updated);
        assert!(fx.dir().join("b.bt").is_file());
    }

    #[test]
    fn branch_tag_and_commit_refs() {
        let Some(fx) = Fixture::new() else { return };
        let first = fx.commit("first.bt", "BEGIN {}");
        fx.git_upstream(&["tag", "v1"]);
        fx.git_upstream(&["checkout", "-q", "-b", "feature"]);
        fx.commit("feature.bt", "BEGIN {}");
        fx.git_upstream(&["checkout", "-q", "main"]);
        fx.commit("later.bt", "BEGIN {}");

        fx.sync(Some("feature")).expect("branch");
        assert!(fx.dir().join("feature.bt").is_file());
        assert!(!fx.dir().join("later.bt").exists());

        fx.sync(Some("v1")).expect("tag");
        assert!(fx.dir().join("first.bt").is_file());
        assert!(!fx.dir().join("feature.bt").exists());

        fx.sync(None).expect("head");
        assert!(fx.dir().join("later.bt").is_file());

        // Abbreviated hash: shallow fetch fails, full fetch + local resolve succeeds.
        fx.sync(Some(&first[..10])).expect("short sha");
        assert!(fx.dir().join("first.bt").is_file());
        assert!(!fx.dir().join("later.bt").exists());

        let fresh = fx.cache.join("fresh");
        fx.git()
            .sync(&fx.spec(Some(&first)), &fresh)
            .expect("full sha, fresh dir");
        assert!(fresh.join("first.bt").is_file());
    }

    #[test]
    fn update_failure_keeps_cached_copy() {
        let Some(fx) = Fixture::new() else { return };
        fx.sync(None).expect("clone");
        fs::remove_dir_all(&fx.upstream).expect("rm upstream");
        match fx.sync(None).expect("stale is not an error") {
            SyncOutcome::Stale { reason } => assert!(reason.contains("fetch"), "{reason}"),
            other => panic!("expected Stale, got {other:?}"),
        }
        assert!(fx.dir().join("a.bt").is_file());
    }

    #[test]
    fn failed_first_clone_leaves_nothing_behind() {
        let Some(fx) = Fixture::new() else { return };
        let err = fx.sync(Some("no-such-branch")).expect_err("must fail");
        assert!(matches!(err, SourceError::Git { .. }), "{err}");
        assert!(!fx.dir().exists());
    }

    #[test]
    fn option_like_ref_is_rejected_before_running_git() {
        let Some(fx) = Fixture::new() else { return };
        let err = fx.sync(Some("--upload-pack=touch pwned")).expect_err("must fail");
        assert!(matches!(err, SourceError::InvalidRef(_)), "{err}");
        assert!(!fx.dir().exists());
    }

    #[test]
    fn hooks_from_user_config_never_run() {
        let Some(fx) = Fixture::new() else { return };
        // A global hooksPath with hooks that fire on fetch/reset (reference-transaction)
        // and checkout. With core.hooksPath=/dev/null forced, none may run.
        let hooks = fx.cache.join("hooks");
        let marker = fx.cache.join("hook-ran");
        fs::create_dir_all(&hooks).expect("mkdir");
        for hook in ["reference-transaction", "post-checkout", "post-merge"] {
            let path = hooks.join(hook);
            fs::write(&path, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).expect("hook");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
            }
        }
        fs::write(
            &fx.global_config,
            format!("[core]\n\thooksPath = {}\n", hooks.display()),
        )
        .expect("config");

        fx.sync(None).expect("clone");
        fx.commit("b.bt", "END {}");
        fx.sync(None).expect("update");
        assert!(!marker.exists(), "a hook was executed");

        // Positive control: without the forced config the same setup does run the hook.
        let status = Command::new("git")
            .arg("-C")
            .arg(fx.dir())
            .args(["update-ref", "refs/heads/probe", "HEAD"])
            .envs(fx.env())
            .status()
            .expect("git");
        assert!(status.success());
        assert!(
            marker.exists(),
            "control: hook setup is not effective, test proves nothing"
        );
    }
}
