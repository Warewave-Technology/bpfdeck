//! Resolve the command-line source (local path or git URL) to a local directory (spec §6.1).

pub mod git;

use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

pub use git::{GitSpec, SyncOutcome};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSpec {
    Local(PathBuf),
    Git(GitSpec),
}

impl SourceSpec {
    /// Git if the input looks like a URL git understands, otherwise a local path.
    /// `#ref` is only split off git URLs; local paths may legitimately contain `#`.
    pub fn parse(input: &str) -> Self {
        const GIT_PREFIXES: &[&str] = &["https://", "http://", "ssh://", "git://", "file://", "git@"];
        if !GIT_PREFIXES.iter().any(|p| input.starts_with(p)) {
            return Self::Local(PathBuf::from(input));
        }
        let (url, reference) = match input.rsplit_once('#') {
            Some((url, r)) if !r.is_empty() => (url, Some(r.to_string())),
            Some((url, _)) => (url, None),
            None => (input, None),
        };
        Self::Git(GitSpec {
            url: url.to_string(),
            reference,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Local,
    Git { spec: GitSpec, outcome: SyncOutcome },
}

#[derive(Debug, Clone)]
pub struct ResolvedSource {
    pub origin: Origin,
    /// Directory the script IDs are relative to (canonical).
    pub root: PathBuf,
    /// Set when the input was a single script file (inside `root`).
    pub file: Option<PathBuf>,
    /// Non-fatal problems to show to the user (e.g. update failed, using cache).
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("{0} does not exist")]
    NotFound(PathBuf),
    #[error("{0} is neither a directory nor a regular file")]
    NotDirOrFile(PathBuf),
    #[error("cannot access {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("no cache directory available (is $HOME set?)")]
    NoCacheDir,
    #[error("invalid git ref {0:?}")]
    InvalidRef(String),
    #[error("cannot run git: {0}")]
    GitMissing(io::Error),
    #[error("git {step} failed for {url}: {stderr}")]
    Git {
        step: String,
        url: String,
        stderr: String,
    },
}

/// `$XDG_CACHE_HOME/bpfdeck` (or the platform equivalent).
pub fn default_cache_root() -> Result<PathBuf, SourceError> {
    ProjectDirs::from("", "", "bpfdeck")
        .map(|d| d.cache_dir().to_path_buf())
        .ok_or(SourceError::NoCacheDir)
}

/// Resolve `input` to a local directory, cloning or updating git sources under
/// `cache_root/repos/`. Blocking (runs `git`); call it off the UI thread.
pub fn resolve(input: &str, cache_root: &Path) -> Result<ResolvedSource, SourceError> {
    match SourceSpec::parse(input) {
        SourceSpec::Local(path) => resolve_local(&path),
        SourceSpec::Git(spec) => {
            let dir = git::repo_dir(cache_root, &spec.url);
            let outcome = git::Git::default().sync(&spec, &dir)?;
            let root = std::fs::canonicalize(&dir).map_err(|source| SourceError::Io { path: dir, source })?;
            let warnings = match &outcome {
                SyncOutcome::Stale { reason } => {
                    vec![format!(
                        "could not update {}, using cached copy: {reason}",
                        spec.url
                    )]
                }
                _ => Vec::new(),
            };
            Ok(ResolvedSource {
                origin: Origin::Git { spec, outcome },
                root,
                file: None,
                warnings,
            })
        }
    }
}

fn resolve_local(path: &Path) -> Result<ResolvedSource, SourceError> {
    let real = std::fs::canonicalize(path).map_err(|source| match source.kind() {
        io::ErrorKind::NotFound => SourceError::NotFound(path.to_path_buf()),
        _ => SourceError::Io {
            path: path.to_path_buf(),
            source,
        },
    })?;
    let meta = std::fs::metadata(&real).map_err(|source| SourceError::Io {
        path: real.clone(),
        source,
    })?;
    let (root, file) = if meta.is_dir() {
        (real, None)
    } else if meta.is_file() {
        let parent = real
            .parent()
            .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
        (parent, Some(real))
    } else {
        return Err(SourceError::NotDirOrFile(real));
    };
    Ok(ResolvedSource {
        origin: Origin::Local,
        root,
        file,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scripts");

    fn git(url: &str, reference: Option<&str>) -> SourceSpec {
        SourceSpec::Git(GitSpec {
            url: url.into(),
            reference: reference.map(Into::into),
        })
    }

    #[test]
    fn parse_inputs() {
        let cases = [
            ("tests/fixtures", SourceSpec::Local("tests/fixtures".into())),
            ("./dir#with-hash", SourceSpec::Local("./dir#with-hash".into())),
            ("/abs/file.bt", SourceSpec::Local("/abs/file.bt".into())),
            (
                "https://github.com/bpftrace/bpftrace",
                git("https://github.com/bpftrace/bpftrace", None),
            ),
            (
                "https://github.com/bpftrace/bpftrace#v0.27.0",
                git("https://github.com/bpftrace/bpftrace", Some("v0.27.0")),
            ),
            (
                "https://example.com/r.git#",
                git("https://example.com/r.git", None),
            ),
            (
                "git@github.com:org/repo.git#main",
                git("git@github.com:org/repo.git", Some("main")),
            ),
            (
                "ssh://git@host:2222/r.git",
                git("ssh://git@host:2222/r.git", None),
            ),
            (
                "file:///srv/mirror/r.git#abc1234",
                git("file:///srv/mirror/r.git", Some("abc1234")),
            ),
        ];
        for (input, want) in cases {
            assert_eq!(SourceSpec::parse(input), want, "{input}");
        }
    }

    #[test]
    fn local_dir_and_file() {
        let dir = resolve(FIXTURES, Path::new("/unused")).expect("dir");
        assert_eq!(dir.origin, Origin::Local);
        assert_eq!(dir.file, None);
        assert!(dir.root.ends_with("tests/fixtures/scripts"));

        let file = resolve(
            &format!("{FIXTURES}/net/tcpconnect_demo.bt"),
            Path::new("/unused"),
        )
        .expect("file");
        assert!(file.root.ends_with("tests/fixtures/scripts/net"));
        assert!(
            file.file
                .as_deref()
                .is_some_and(|f| f.ends_with("net/tcpconnect_demo.bt"))
        );
    }

    #[test]
    fn local_missing() {
        assert!(matches!(
            resolve("/definitely/not/here", Path::new("/unused")),
            Err(SourceError::NotFound(_))
        ));
    }
}
