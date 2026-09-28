//! Script discovery and metadata extraction (spec §6.2, §6.3). std only, no ratatui/tokio.

pub mod lexer;
pub mod metadata;

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

/// Files larger than this are never scripts worth listing.
pub const MAX_SCRIPT_SIZE: u64 = 1024 * 1024;
/// Safety net on top of symlink loop detection.
const MAX_DEPTH: usize = 64;
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptFile {
    /// Stable ID: path relative to the root, `/`-separated.
    pub id: String,
    /// Canonical path on disk (symlinks resolved).
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Default)]
pub struct Discovery {
    /// Sorted by `id`.
    pub scripts: Vec<ScriptFile>,
    /// Non-fatal problems (unreadable subdirectories, broken symlinks, …).
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("cannot read {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{0} is not a bpftrace script (expected a .bt file or a bpftrace shebang)")]
    NotAScript(PathBuf),
    #[error("{path} is larger than {MAX_SCRIPT_SIZE} bytes")]
    TooLarge { path: PathBuf },
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> DiscoveryError + '_ {
    move |source| DiscoveryError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Recursively find scripts under `root`.
///
/// Skips `.git`, `target`, `node_modules` and hidden directories, files over
/// [`MAX_SCRIPT_SIZE`], symlinks that leave the root, and symlink loops. A file reachable
/// through several paths (symlinks inside the root) is listed once, preferring a path
/// without symlinks, then the smallest ID.
pub fn walk(root: &Path) -> Result<Discovery, DiscoveryError> {
    let root = fs::canonicalize(root).map_err(io_err(root))?;
    let mut out = Discovery::default();

    // Scripts paired with "reached through a symlink", for deduplication.
    let mut found: Vec<(bool, ScriptFile)> = Vec::new();
    // (canonical dir, relative path, canonical ancestors including itself, via symlink)
    let mut stack = vec![(root.clone(), PathBuf::new(), vec![root.clone()], false)];
    while let Some((dir, rel, ancestors, via_link)) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if dir == root => return Err(io_err(&root)(e)),
            Err(e) => {
                out.warnings.push(format!("skipped {}: {e}", rel.display()));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    out.warnings
                        .push(format!("skipped an entry in {}: {e}", dir.display()));
                    continue;
                }
            };
            let name = entry.file_name();
            let rel_child = rel.join(&name);
            let path = entry.path();
            let is_symlink = entry.file_type().is_ok_and(|t| t.is_symlink());
            // Everything below `root` is canonical by construction; only symlinks need resolving.
            let real = if is_symlink {
                match fs::canonicalize(&path) {
                    Ok(real) if real.starts_with(&root) => real,
                    Ok(_) => continue, // points outside the root: never follow
                    Err(e) => {
                        out.warnings
                            .push(format!("broken symlink {}: {e}", rel_child.display()));
                        continue;
                    }
                }
            } else {
                path
            };
            let meta = match fs::metadata(&real) {
                Ok(meta) => meta,
                Err(e) => {
                    out.warnings.push(format!("skipped {}: {e}", rel_child.display()));
                    continue;
                }
            };

            if meta.is_dir() {
                let name = name.to_string_lossy();
                if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                if ancestors.contains(&real) || ancestors.len() >= MAX_DEPTH {
                    continue; // symlink loop
                }
                let mut child_ancestors = ancestors.clone();
                child_ancestors.push(real.clone());
                stack.push((real, rel_child, child_ancestors, via_link || is_symlink));
            } else if meta.is_file() && meta.len() <= MAX_SCRIPT_SIZE && is_candidate(&real) {
                let script = ScriptFile {
                    id: to_id(&rel_child),
                    path: real,
                    size: meta.len(),
                };
                found.push((via_link || is_symlink, script));
            }
        }
    }

    found.sort_by(|(la, a), (lb, b)| la.cmp(lb).then_with(|| a.id.cmp(&b.id)));
    let mut seen = HashSet::new();
    out.scripts = found
        .into_iter()
        .filter_map(|(_, s)| seen.insert(s.path.clone()).then_some(s))
        .collect();
    out.scripts.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Discovery result for a single script given directly on the command line.
pub fn single_file(path: &Path) -> Result<Discovery, DiscoveryError> {
    let real = fs::canonicalize(path).map_err(io_err(path))?;
    let meta = fs::metadata(&real).map_err(io_err(&real))?;
    if meta.len() > MAX_SCRIPT_SIZE {
        return Err(DiscoveryError::TooLarge { path: real });
    }
    if !is_candidate(&real) {
        return Err(DiscoveryError::NotAScript(real));
    }
    let id = real.file_name().map_or_else(
        || real.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    Ok(Discovery {
        scripts: vec![ScriptFile {
            id,
            path: real,
            size: meta.len(),
        }],
        warnings: Vec::new(),
    })
}

/// `*.bt`, or a first line that is a shebang mentioning bpftrace.
pub fn is_candidate(path: &Path) -> bool {
    if path.extension().is_some_and(|e| e == "bt") {
        return true;
    }
    let mut head = [0u8; 256];
    let n = fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    has_bpftrace_shebang(&head[..n])
}

fn has_bpftrace_shebang(head: &[u8]) -> bool {
    let first_line = head.split(|&b| b == b'\n').next().unwrap_or_default();
    first_line.starts_with(b"#!") && first_line.windows(8).any(|w| w == b"bpftrace")
}

fn to_id(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scripts");

    fn ids(d: &Discovery) -> Vec<&str> {
        d.scripts.iter().map(|s| s.id.as_str()).collect()
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, content).expect("write");
    }

    #[test]
    fn fixture_scripts() {
        let d = walk(Path::new(FIXTURES)).expect("walk");
        assert_eq!(
            ids(&d),
            vec![
                "missing_probe_demo.bt",
                "net/tcpconnect_demo.bt",
                "no_header.bt",
                "params_demo.bt",
                "shebang_no_ext",
                "syscount_demo.bt",
                "unsafe_demo.bt",
                "vfs_latency_demo.bt",
            ]
        );
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
        assert!(d.scripts.iter().all(|s| s.path.is_absolute() && s.size > 0));
    }

    #[test]
    fn skips_ignored_dirs_and_large_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let r = tmp.path();
        write(&r.join("ok.bt"), "BEGIN {}");
        write(&r.join(".hidden/x.bt"), "BEGIN {}");
        write(&r.join(".git/x.bt"), "BEGIN {}");
        write(&r.join("target/x.bt"), "BEGIN {}");
        write(&r.join("node_modules/x.bt"), "BEGIN {}");
        write(&r.join(".dotfile.bt"), "BEGIN {}"); // hidden *files* are not skipped
        write(&r.join("big.bt"), &"x".repeat(MAX_SCRIPT_SIZE as usize + 1));
        write(&r.join("exact.bt"), &"x".repeat(MAX_SCRIPT_SIZE as usize));
        write(&r.join("script.sh"), "#!/bin/sh\necho bpftrace\n");
        write(&r.join("tool"), "#!/usr/bin/bpftrace -q\nBEGIN {}");
        write(&r.join("empty"), "");
        let d = walk(r).expect("walk");
        assert_eq!(ids(&d), vec![".dotfile.bt", "exact.bt", "ok.bt", "tool"]);
    }

    #[test]
    fn shebang_detection() {
        assert!(has_bpftrace_shebang(b"#!/usr/bin/env bpftrace\n"));
        assert!(has_bpftrace_shebang(b"#!/usr/local/bin/bpftrace"));
        assert!(!has_bpftrace_shebang(b"#!/bin/bash\n# uses bpftrace\n"));
        assert!(!has_bpftrace_shebang(b"// bpftrace\n"));
        assert!(!has_bpftrace_shebang(b""));
    }

    #[test]
    fn single_file_input() {
        let d = single_file(&Path::new(FIXTURES).join("shebang_no_ext")).expect("single");
        assert_eq!(ids(&d), vec!["shebang_no_ext"]);
        assert!(matches!(
            single_file(&Path::new(FIXTURES).join("README.txt")),
            Err(DiscoveryError::NotAScript(_))
        ));
        assert!(matches!(
            single_file(&Path::new(FIXTURES).join("nope.bt")),
            Err(DiscoveryError::Io { .. })
        ));
    }

    #[test]
    fn missing_root_is_an_error() {
        assert!(matches!(
            walk(Path::new("/definitely/not/here")),
            Err(DiscoveryError::Io { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_loops_outside_and_duplicates() {
        use std::os::unix::fs::symlink;
        let outside = tempfile::tempdir().expect("tempdir");
        write(&outside.path().join("secret.bt"), "BEGIN {}");
        let tmp = tempfile::tempdir().expect("tempdir");
        let r = tmp.path();
        write(&r.join("a/one.bt"), "BEGIN {}");
        write(&r.join("b/two.bt"), "BEGIN {}");
        symlink(r, r.join("a/loop_to_root")).expect("symlink");
        symlink(r.join("b"), r.join("a/to_b")).expect("symlink");
        symlink(r.join("a"), r.join("b/to_a")).expect("symlink");
        symlink(outside.path(), r.join("outside_dir")).expect("symlink");
        symlink(outside.path().join("secret.bt"), r.join("outside.bt")).expect("symlink");
        symlink(r.join("a/one.bt"), r.join("alias.bt")).expect("symlink");
        symlink(r.join("missing.bt"), r.join("broken.bt")).expect("symlink");

        let d = walk(r).expect("walk");
        // Each real file once, under its symlink-free ID; nothing from outside the root.
        assert_eq!(ids(&d), vec!["a/one.bt", "b/two.bt"]);
        assert_eq!(d.warnings.len(), 1, "{:?}", d.warnings);
        assert!(d.warnings[0].contains("broken.bt"));
    }
}
