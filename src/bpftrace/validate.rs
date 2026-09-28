//! Background validation of scripts against this kernel (spec §6.4, D-006).
//!
//! Two strategies, chosen once at startup: `--dry-run` (authoritative, needs privileges
//! and a recent bpftrace) or `-l` per probe (heuristic fallback). Results are cached by
//! (content hash, bpftrace version, kernel release); `-l` answers by probe pattern.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use super::{BpftraceInfo, CaptureError, capture, command};
use crate::discovery::metadata::Metadata;
use crate::sys::Privilege;

pub const DEFAULT_WORKERS: usize = 4;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Strategy {
    DryRun,
    ProbeList,
}

impl Strategy {
    pub fn choose(info: &BpftraceInfo, privilege: Privilege) -> Self {
        if info.supports_dry_run && privilege != Privilege::None {
            Self::DryRun
        } else {
            Self::ProbeList
        }
    }
}

/// Everything validation needs to know about one script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationRequest {
    pub path: PathBuf,
    /// sha256 of the script content, hex.
    pub content_hash: String,
    /// Highest `$N` the script reads; the dry run passes that many placeholders.
    pub positional_count: u32,
    /// Probe specs that need a lookup (always-available ones like `BEGIN` excluded).
    pub probes: Vec<String>,
    /// Metadata hint: the script calls `system()` & co.
    pub needs_unsafe: bool,
}

impl ValidationRequest {
    pub fn new(path: &Path, content: &str, meta: &Metadata) -> Self {
        Self {
            path: path.to_path_buf(),
            content_hash: hex::encode(Sha256::digest(content.as_bytes())),
            positional_count: meta.params.positional.last().copied().unwrap_or(0),
            probes: meta
                .probes
                .iter()
                .filter(|p| !p.always_available)
                .map(|p| p.spec.clone())
                .collect(),
            needs_unsafe: meta.needs_unsafe(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    pub verdict: Verdict,
    pub strategy: Strategy,
    /// Raw bpftrace stderr/stdout for the Validation tab, verbatim.
    pub output: String,
    /// Things the user should know about how the result was obtained.
    pub notes: Vec<String>,
    /// Per-probe results (probe-list strategy only).
    pub probes: Vec<ProbeCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// `●` dry run passed / all probes found.
    Ok,
    /// `◐` some probes of the script were not found.
    Partial { found: usize, total: usize },
    /// `!` bpftrace refuses without `--unsafe`.
    NeedsUnsafe,
    /// `✗` cannot run here.
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeCheck {
    pub probe: String,
    pub found: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    content_hash: String,
    bpftrace_version: String,
    kernel_release: String,
}

pub struct Validator {
    bpftrace: PathBuf,
    bpftrace_version: String,
    kernel_release: String,
    strategy: Strategy,
    timeout: Duration,
    workers: Semaphore,
    cache: Mutex<HashMap<CacheKey, Validation>>,
    probe_cache: Mutex<HashMap<String, bool>>,
}

impl Validator {
    pub fn new(
        info: &BpftraceInfo,
        kernel_release: &str,
        strategy: Strategy,
        workers: usize,
        timeout: Duration,
    ) -> Self {
        Self {
            bpftrace: info.path.clone(),
            bpftrace_version: info.version_raw.clone(),
            kernel_release: kernel_release.to_string(),
            strategy,
            timeout,
            workers: Semaphore::new(workers.max(1)),
            cache: Mutex::new(HashMap::new()),
            probe_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Validate one script. At most `workers` validations run bpftrace at a time; cached
    /// results return immediately.
    pub async fn validate(&self, req: &ValidationRequest) -> Validation {
        let key = CacheKey {
            content_hash: req.content_hash.clone(),
            bpftrace_version: self.bpftrace_version.clone(),
            kernel_release: self.kernel_release.clone(),
        };
        if let Some(hit) = lock(&self.cache).get(&key) {
            return hit.clone();
        }
        let Ok(_permit) = self.workers.acquire().await else {
            return failed(self.strategy, "validator shut down".into());
        };
        let result = match self.strategy {
            Strategy::DryRun => self.dry_run(req).await,
            Strategy::ProbeList => self.probe_list(req).await,
        };
        // Timeouts and spawn failures say nothing about the script: don't cache them.
        if result.notes.iter().all(|n| !n.starts_with(TRANSIENT)) {
            lock(&self.cache).insert(key, result.clone());
        }
        result
    }

    async fn dry_run(&self, req: &ValidationRequest) -> Validation {
        let argv = command::dry_run_argv(&self.bpftrace, &req.path, req.positional_count);
        let mut notes = Vec::new();
        if req.positional_count > 0 {
            notes.push(format!(
                "positional parameters $1..${} were set to 0 for the dry run",
                req.positional_count
            ));
        }
        let out = match capture(&argv, self.timeout).await {
            Ok(out) => out,
            Err(e) => return self.transient(e, notes),
        };
        let output = out.combined();
        let verdict = if out.success {
            Verdict::Ok
        } else if needs_unsafe(&output) {
            Verdict::NeedsUnsafe
        } else {
            Verdict::Failed {
                reason: failure_reason(&output, out.code),
            }
        };
        Validation {
            verdict,
            strategy: Strategy::DryRun,
            output,
            notes,
            probes: Vec::new(),
        }
    }

    async fn probe_list(&self, req: &ValidationRequest) -> Validation {
        let mut notes = vec!["heuristic: probes looked up with `bpftrace -l`, script not loaded".to_string()];
        let mut output = String::new();
        let mut probes = Vec::with_capacity(req.probes.len());
        for probe in &req.probes {
            let cached = lock(&self.probe_cache).get(probe).copied();
            let found = match cached {
                Some(found) => found,
                None => {
                    let argv = command::probe_list_argv(&self.bpftrace, probe);
                    let out = match capture(&argv, self.timeout).await {
                        Ok(out) => out,
                        Err(e) => return self.transient(e, notes),
                    };
                    let found = out.success && out.stdout.lines().any(|l| !l.trim().is_empty());
                    if !out.stderr.trim().is_empty() {
                        output.push_str(&format!("$ bpftrace -l '{probe}'\n{}\n", out.stderr.trim()));
                    }
                    lock(&self.probe_cache).insert(probe.clone(), found);
                    found
                }
            };
            probes.push(ProbeCheck {
                probe: probe.clone(),
                found,
            });
        }

        let total = probes.len();
        let found = probes.iter().filter(|p| p.found).count();
        let verdict = if total > 0 && found == 0 {
            let missing: Vec<_> = probes.iter().map(|p| p.probe.as_str()).collect();
            Verdict::Failed {
                reason: format!("no probe found: {}", missing.join(", ")),
            }
        } else if found < total {
            Verdict::Partial { found, total }
        } else if req.needs_unsafe {
            Verdict::NeedsUnsafe
        } else {
            Verdict::Ok
        };
        if req.needs_unsafe {
            notes.push("script calls unsafe functions; running it needs --unsafe".to_string());
        }
        Validation {
            verdict,
            strategy: Strategy::ProbeList,
            output,
            notes,
            probes,
        }
    }

    fn transient(&self, e: CaptureError, mut notes: Vec<String>) -> Validation {
        let reason = match e {
            CaptureError::Timeout => format!("timeout after {}s", self.timeout.as_secs_f32()),
            CaptureError::Spawn(e) | CaptureError::Io(e) => {
                format!("cannot run {}: {e}", self.bpftrace.display())
            }
        };
        notes.push(format!("{TRANSIENT} {reason}"));
        failed(self.strategy, reason).with_notes(notes)
    }
}

/// Marks results that must not be cached.
const TRANSIENT: &str = "not cached:";

impl Validation {
    fn with_notes(mut self, notes: Vec<String>) -> Self {
        self.notes = notes;
        self
    }
}

fn failed(strategy: Strategy, reason: String) -> Validation {
    Validation {
        verdict: Verdict::Failed { reason },
        strategy,
        output: String::new(),
        notes: Vec::new(),
        probes: Vec::new(),
    }
}

/// bpftrace's refusal to run unsafe builtins. The wording changed between versions:
/// "…you need the --unsafe flag" (stdlib, ≥ 0.24) and "…is an unsafe function being
/// used in safe mode" (≤ 0.23, verified with Debian 13's 0.23.2).
fn needs_unsafe(output: &str) -> bool {
    output.contains("--unsafe") || output.contains("unsafe function being used in safe mode")
}

/// The first line bpftrace marks as an error, else the first non-empty line.
fn failure_reason(output: &str, code: Option<i32>) -> String {
    let lines = || output.lines().map(str::trim).filter(|l| !l.is_empty());
    lines()
        .find(|l| l.contains("ERROR"))
        .or_else(|| lines().next())
        .map(|l| l.to_string())
        .unwrap_or_else(|| match code {
            Some(code) => format!("exit status {code}"),
            None => "killed by a signal".to_string(),
        })
}

/// A poisoned lock only means another task panicked mid-insert; the map is still usable.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use crate::discovery::metadata;
    use pretty_assertions::assert_eq;

    const SCRIPTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scripts");

    async fn validator(fake: &Path, strategy: Strategy, timeout: Duration) -> Validator {
        let info = super::super::detect(fake).await.expect("detect");
        Validator::new(&info, "6.1.0-test", strategy, DEFAULT_WORKERS, timeout)
    }

    fn request(path: &Path) -> ValidationRequest {
        let content = std::fs::read_to_string(path).expect("read");
        ValidationRequest::new(path, &content, &metadata::extract(&content))
    }

    fn fixture(name: &str) -> ValidationRequest {
        request(&Path::new(SCRIPTS).join(name))
    }

    #[tokio::test]
    async fn strategy_choice() {
        let new = super::super::detect(&fake()).await.expect("detect");
        let old = super::super::detect(&fake_old()).await.expect("detect");
        assert_eq!(Strategy::choose(&new, Privilege::Root), Strategy::DryRun);
        assert_eq!(Strategy::choose(&new, Privilege::Caps), Strategy::DryRun);
        assert_eq!(Strategy::choose(&new, Privilege::None), Strategy::ProbeList);
        assert_eq!(Strategy::choose(&old, Privilege::Root), Strategy::ProbeList);
    }

    #[tokio::test]
    async fn dry_run_verdicts_on_fixtures() {
        let v = validator(&fake(), Strategy::DryRun, DEFAULT_TIMEOUT).await;

        let ok = v.validate(&fixture("syscount_demo.bt")).await;
        assert_eq!(ok.verdict, Verdict::Ok);
        assert_eq!(ok.strategy, Strategy::DryRun);

        let missing = v.validate(&fixture("missing_probe_demo.bt")).await;
        let Verdict::Failed { reason } = &missing.verdict else {
            panic!("{missing:?}")
        };
        assert!(
            reason.contains("kprobe:this_function_does_not_exist_bpfdeck"),
            "{reason}"
        );
        assert!(
            missing.output.contains("No such file or directory"),
            "raw stderr kept"
        );

        let unsafe_demo = v.validate(&fixture("unsafe_demo.bt")).await;
        assert_eq!(unsafe_demo.verdict, Verdict::NeedsUnsafe);

        let params = v.validate(&fixture("params_demo.bt")).await;
        assert_eq!(params.verdict, Verdict::Ok);
        assert_eq!(
            params.notes,
            vec!["positional parameters $1..$1 were set to 0 for the dry run"]
        );
    }

    #[tokio::test]
    async fn probe_list_verdicts() {
        let v = validator(&fake_old(), Strategy::ProbeList, DEFAULT_TIMEOUT).await;

        let ok = v.validate(&fixture("vfs_latency_demo.bt")).await;
        assert_eq!(ok.verdict, Verdict::Ok);
        let checked: Vec<_> = ok.probes.iter().map(|p| (p.probe.as_str(), p.found)).collect();
        assert_eq!(
            checked,
            vec![("kprobe:vfs_read", true), ("kretprobe:vfs_read", true)]
        );
        assert!(ok.notes[0].starts_with("heuristic"));

        let missing = v.validate(&fixture("missing_probe_demo.bt")).await;
        assert!(matches!(missing.verdict, Verdict::Failed { .. }), "{missing:?}");

        let dir = tempfile::tempdir().expect("tempdir");
        let partial = script(&dir, "p.bt", "kprobe:vfs_read, kprobe:does_not_exist_x { }\n");
        assert_eq!(
            v.validate(&request(&partial)).await.verdict,
            Verdict::Partial { found: 1, total: 2 }
        );

        assert_eq!(
            v.validate(&fixture("unsafe_demo.bt")).await.verdict,
            Verdict::NeedsUnsafe
        );
        // Only BEGIN/END/interval: nothing to look up.
        assert_eq!(v.validate(&fixture("shebang_no_ext")).await.verdict, Verdict::Ok);
    }

    #[tokio::test]
    async fn results_are_cached_by_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("argv.log");
        let content = format!("// fake: argv_log={}\nBEGIN {{}}\n", log.display());
        let s = script(&dir, "c.bt", &content);
        let v = validator(&fake(), Strategy::DryRun, DEFAULT_TIMEOUT).await;
        let runs = || std::fs::read_to_string(&log).map_or(0, |l| l.matches("--end--").count());

        v.validate(&request(&s)).await;
        v.validate(&request(&s)).await;
        assert_eq!(runs(), 1, "second validation must hit the cache");

        // Same content under another path: same cache entry. Edited content: new run.
        let copy = script(&dir, "copy.bt", &content);
        v.validate(&request(&copy)).await;
        assert_eq!(runs(), 1);
        std::fs::write(&s, format!("{content}// edited\n")).expect("edit");
        v.validate(&request(&s)).await;
        assert_eq!(runs(), 2);
    }

    #[tokio::test]
    async fn timeout_is_a_failure_and_not_cached() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = script(&dir, "slow.bt", "// fake: dryrun_sleep=5\nBEGIN {}\n");
        let v = validator(&fake(), Strategy::DryRun, Duration::from_millis(300)).await;
        let started = std::time::Instant::now();
        let result = v.validate(&request(&s)).await;
        assert!(started.elapsed() < Duration::from_secs(3));
        let Verdict::Failed { reason } = &result.verdict else {
            panic!("{result:?}")
        };
        assert!(reason.starts_with("timeout"), "{reason}");
        assert!(lock(&v.cache).is_empty());
    }

    #[tokio::test]
    async fn worker_pool_is_bounded() {
        // 4 workers, 8 scripts sleeping 0.4s each: two waves, so ≥ 0.8s but well under 8×.
        let dir = tempfile::tempdir().expect("tempdir");
        let v = std::sync::Arc::new(validator(&fake(), Strategy::DryRun, DEFAULT_TIMEOUT).await);
        let mut set = tokio::task::JoinSet::new();
        let started = std::time::Instant::now();
        for i in 0..8 {
            let content = format!("// fake: dryrun_sleep=0.4\n// {i}\nBEGIN {{}}\n");
            let req = request(&script(&dir, &format!("s{i}.bt"), &content));
            let v = v.clone();
            set.spawn(async move { v.validate(&req).await });
        }
        while let Some(result) = set.join_next().await {
            assert_eq!(result.expect("task").verdict, Verdict::Ok);
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(800),
            "{elapsed:?}: more than 4 ran at once"
        );
        assert!(elapsed < Duration::from_millis(3000), "{elapsed:?}: ran serially");
    }

    #[test]
    fn unsafe_refusals_across_versions() {
        assert!(needs_unsafe(
            "stdin:7:3-17: ERROR: system() is unsafe. To use you need the --unsafe flag"
        ));
        assert!(needs_unsafe(
            "/s/unsafe_demo.bt:7:3-19: ERROR: system() is an unsafe function being used in safe mode"
        ));
        assert!(!needs_unsafe(
            "ERROR: tracepoint not found: sched:sched_process_exec"
        ));
    }

    #[test]
    fn failure_reasons() {
        assert_eq!(
            failure_reason("Attaching…\nstdin:1:1: ERROR: bad thing\nmore", Some(1)),
            "stdin:1:1: ERROR: bad thing"
        );
        assert_eq!(failure_reason("\n  something odd\n", Some(1)), "something odd");
        assert_eq!(failure_reason("", Some(2)), "exit status 2");
        assert_eq!(failure_reason("", None), "killed by a signal");
    }
}
