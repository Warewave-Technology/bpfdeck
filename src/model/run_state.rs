//! One bpftrace run as the UI sees it (spec §5.4): phase, counters, log, panels.
//! Time is passed in by the caller so everything here is deterministic.

use std::time::{Duration, Instant};

use super::describe::describe;
use super::log::{DEFAULT_CAPACITY, LogBuffer, LogKind};
use super::panels::Panels;
use crate::bpftrace::json::OutputMsg;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Spawn requested, no confirmation yet.
    Starting,
    Running,
    /// SIGINT sent; waiting for the exit-time dump and exit.
    Stopping,
    Exited,
    /// Could not be started at all.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    /// Name of the signal the stop had to escalate to (`SIGTERM`, `SIGKILL`).
    pub forced: Option<String>,
    pub error: Option<String>,
}

impl ExitInfo {
    pub fn describe(&self) -> String {
        let mut s = match (self.code, self.signal) {
            (Some(code), _) => format!("exited({code})"),
            (None, Some(sig)) => format!("killed by signal {sig}"),
            (None, None) => "exited".to_string(),
        };
        if let Some(forced) = &self.forced {
            s.push_str(&format!(", escalated to {forced}"));
        }
        if let Some(e) = &self.error {
            s.push_str(&format!(", {e}"));
        }
        s
    }
}

#[derive(Debug, Clone)]
pub struct Run {
    pub id: u64,
    pub script_id: String,
    /// The command line as shown in the confirmation dialog.
    pub command: String,
    pub phase: Phase,
    pub exit: Option<ExitInfo>,
    pub failure: Option<String>,
    started: Option<Instant>,
    pub elapsed: Duration,
    /// From the `attached_probes` message.
    pub attached_probes: Option<u64>,
    /// stderr lines + helper errors.
    pub errors: u64,
    /// Events dropped under load (see spec §6.5); counted by the runner.
    pub dropped: u64,
    pub log: LogBuffer,
    /// Latest message per map name; each new print replaces the previous snapshot.
    pub panels: Panels,
}

impl Run {
    pub fn new(id: u64, script_id: &str, command: &str) -> Self {
        let mut log = LogBuffer::new(DEFAULT_CAPACITY);
        log.push_line(LogKind::System, &format!("$ {command}"));
        Self {
            id,
            script_id: script_id.to_string(),
            command: command.to_string(),
            phase: Phase::Starting,
            exit: None,
            failure: None,
            started: None,
            elapsed: Duration::ZERO,
            attached_probes: None,
            errors: 0,
            dropped: 0,
            log,
            panels: Panels::default(),
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self.phase, Phase::Starting | Phase::Running | Phase::Stopping)
    }

    pub fn started(&mut self, at: Instant) {
        self.started = Some(at);
        if self.phase == Phase::Starting {
            self.phase = Phase::Running;
        }
    }

    pub fn tick(&mut self, now: Instant) {
        if self.is_active()
            && let Some(started) = self.started
        {
            self.elapsed = now.saturating_duration_since(started);
        }
    }

    /// Run time at `at` (zero before the start was confirmed).
    fn run_time(&self, at: Instant) -> Duration {
        self.started
            .map_or(Duration::ZERO, |s| at.saturating_duration_since(s))
    }

    pub fn output(&mut self, msg: OutputMsg, at: Instant) {
        let now = self.run_time(at);
        if self.panels.apply(&msg, now) {
            return;
        }
        match &msg {
            OutputMsg::AttachedProbes(n) => {
                self.attached_probes = Some(*n);
                self.log.push_line(LogKind::System, &describe(&msg));
            }
            OutputMsg::Text { text, .. } => self.log.push_text(LogKind::Output, text),
            OutputMsg::Value(_) => self.log.push_line(LogKind::Output, &describe(&msg)),
            OutputMsg::HelperError { .. } => {
                self.errors += 1;
                self.log.push_line(LogKind::Error, &describe(&msg));
            }
            // Panel messages were taken by `panels.apply` above.
            OutputMsg::Map { .. }
            | OutputMsg::Hist { .. }
            | OutputMsg::Stats { .. }
            | OutputMsg::Tseries { .. } => {}
            OutputMsg::Unknown(_) | OutputMsg::NotJson(_) => {
                self.log.push_line(LogKind::Raw, &describe(&msg))
            }
        }
    }

    pub fn stderr(&mut self, line: &str) {
        self.errors += 1;
        self.log.push_line(LogKind::Error, line);
    }

    /// Stop requested (SIGINT sent). No-op unless running.
    pub fn stopping(&mut self) -> bool {
        if !matches!(self.phase, Phase::Starting | Phase::Running) {
            return false;
        }
        self.phase = Phase::Stopping;
        self.log.push_line(
            LogKind::System,
            "stopping: SIGINT sent, waiting for END and the final map dump…",
        );
        true
    }

    pub fn exited(&mut self, exit: ExitInfo, at: Instant) {
        self.tick(at);
        self.panels.focus_final();
        self.log.push_line(LogKind::System, &exit.describe());
        self.exit = Some(exit);
        self.phase = Phase::Exited;
    }

    pub fn failed(&mut self, reason: &str) {
        self.log
            .push_line(LogKind::Error, &format!("cannot start: {reason}"));
        self.failure = Some(reason.to_string());
        self.phase = Phase::Failed;
    }

    /// `running`, `exited(0)`, … for the header.
    pub fn state_label(&self) -> String {
        match self.phase {
            Phase::Starting => "starting".into(),
            Phase::Running => "running".into(),
            Phase::Stopping => "stopping".into(),
            Phase::Exited => self.exit.as_ref().map_or_else(
                || "exited".into(),
                |e| match e.code {
                    Some(code) => format!("exited({code})"),
                    None => e
                        .signal
                        .map_or_else(|| "exited".into(), |s| format!("killed({s})")),
                },
            ),
            Phase::Failed => "failed".into(),
        }
    }

    /// Exited with status 0.
    pub fn succeeded(&self) -> bool {
        self.phase == Phase::Exited && self.exit.as_ref().is_some_and(|e| e.code == Some(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpftrace::json::parse_line;
    use pretty_assertions::assert_eq;

    fn feed(run: &mut Run, ndjson: &str) {
        for line in ndjson.lines() {
            for msg in parse_line(line) {
                run.output(msg, Instant::now());
            }
        }
    }

    fn log(run: &Run) -> Vec<(LogKind, String)> {
        run.log
            .matching("")
            .iter()
            .map(|l| (l.kind, l.text.clone()))
            .collect()
    }

    #[test]
    fn lifecycle_with_exit_time_dump() {
        let t0 = Instant::now();
        let mut run = Run::new(1, "vfs_latency_demo.bt", "bpftrace -f json -B line -- /s/vfs.bt");
        assert_eq!(run.phase, Phase::Starting);
        assert!(run.is_active());
        run.started(t0);
        assert_eq!(run.state_label(), "running");

        let session = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/json/session_mixed.ndjson"
        ))
        .expect("fixture");
        feed(&mut run, &session);
        assert_eq!(run.attached_probes, Some(3));
        assert_eq!(run.panels.list.len(), 2, "@syscalls and @usecs, latest only");
        run.tick(t0 + Duration::from_secs(12));
        assert_eq!(run.elapsed, Duration::from_secs(12));

        assert!(run.stopping());
        assert!(!run.stopping(), "second stop is a no-op");
        // bpftrace prints remaining maps on SIGINT: they must still be taken in.
        feed(
            &mut run,
            r#"{"type":"hist","data":{"@usecs":[{"min":0,"max":0,"count":9}]}}"#,
        );
        run.exited(
            ExitInfo {
                code: Some(0),
                signal: None,
                forced: None,
                error: None,
            },
            t0 + Duration::from_secs(13),
        );
        assert!(run.succeeded());
        assert!(!run.is_active());
        assert_eq!(run.elapsed, Duration::from_secs(13));
        assert_eq!(run.state_label(), "exited(0)");
        let lines = log(&run);
        assert_eq!(
            lines[0],
            (LogKind::System, "$ bpftrace -f json -B line -- /s/vfs.bt".into())
        );
        assert_eq!(lines[1], (LogKind::System, "attached 3 probes".into()));
        assert_eq!(
            lines[2],
            (
                LogKind::Output,
                "Tracing block device I/O... Hit Ctrl-C to end.".into()
            )
        );
        assert_eq!(lines.last(), Some(&(LogKind::System, "exited(0)".into())));
        assert!(
            !lines.iter().any(|(_, t)| t.starts_with("hist ")),
            "snapshots go to panels, not the log"
        );
        assert_eq!(
            run.panels.list[1].selected_hist().map(|b| b[0].count),
            Some(9),
            "exit-time dump kept"
        );

        // Time stops once the run is over.
        run.tick(t0 + Duration::from_secs(60));
        assert_eq!(run.elapsed, Duration::from_secs(13));
    }

    /// Real bpftrace 0.23.2 output (docs/bpftrace-json.md checklist): no `count` in
    /// attached_probes, blank lines before the exit dump, helper errors mid-stream.
    #[test]
    fn real_readlat_session() {
        let t0 = Instant::now();
        let mut run = Run::new(1, "readlat.bt", "bpftrace -f json -B line -- readlat.bt");
        run.started(t0);
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/json/real_debian13_orbstack_readlat.ndjson"
        ))
        .expect("fixture");
        feed(&mut run, &text);
        assert_eq!(run.attached_probes, Some(3));
        assert_eq!(run.errors, 3);
        let hist = run
            .panels
            .list
            .first()
            .and_then(|p| p.selected_hist())
            .expect("exit-time hist");
        assert_eq!(hist.first().map(|b| b.count), Some(422));
    }

    #[test]
    fn errors_are_counted_and_logged() {
        let mut run = Run::new(1, "x", "cmd");
        run.started(Instant::now());
        run.stderr("WARNING: something");
        feed(
            &mut run,
            r#"{"type": "helper_error", "msg": "Bad address", "helper": "probe_read_user", "line": 1}"#,
        );
        feed(&mut run, "not json at all\n{\"type\":\"future\"}");
        assert_eq!(run.errors, 2);
        let kinds: Vec<LogKind> = log(&run).into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            kinds,
            vec![
                LogKind::System,
                LogKind::Error,
                LogKind::Error,
                LogKind::Raw,
                LogKind::Raw
            ]
        );
    }

    #[test]
    fn escalated_and_failed_runs() {
        let mut run = Run::new(1, "x", "cmd");
        run.started(Instant::now());
        run.stopping();
        run.exited(
            ExitInfo {
                code: None,
                signal: Some(9),
                forced: Some("SIGKILL".into()),
                error: None,
            },
            Instant::now(),
        );
        assert_eq!(run.state_label(), "killed(9)");
        assert!(!run.succeeded());
        assert_eq!(
            log(&run).last().map(|(_, t)| t.as_str()),
            Some("killed by signal 9, escalated to SIGKILL")
        );

        let mut run = Run::new(2, "x", "cmd");
        run.failed("No such file or directory");
        assert_eq!(run.state_label(), "failed");
        assert!(!run.is_active());
        assert!(!run.stopping());
    }
}
