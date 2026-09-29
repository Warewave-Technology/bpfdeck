//! `Msg`: everything that happens to the app. `Cmd`: side effects the app asks for.
//! See docs/architecture.md ("Concurrency model").

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Instant;

use ratatui::crossterm::event::KeyEvent;

use crate::app::target::TargetId;
use crate::bpftrace::BpftraceInfo;
use crate::bpftrace::coalesce::Batch;
use crate::bpftrace::validate::{Strategy, Validation, ValidationRequest};
use crate::catalog::Catalog;
use crate::remote::Dest;
use crate::remote::connect::{Check, SudoChoice};
use crate::remote::facts::RemoteInfo;
use crate::source::ResolvedSource;
use crate::sys::SystemInfo;

#[derive(Debug)]
pub enum Msg {
    Key(KeyEvent),
    /// Terminal resized: redraw.
    Resize,
    /// SIGTERM/SIGHUP: leave cleanly.
    Terminate,
    /// Result of `Cmd::Load` / `Cmd::Rescan`.
    Loaded(Result<Catalog, String>),
    /// Result of `Cmd::DetectEnv`. `bpftrace` is `Err` when it can't be run here.
    EnvDetected {
        target: TargetId,
        host: SystemInfo,
        bpftrace: Result<(BpftraceInfo, Strategy), String>,
    },
    Validated {
        target: TargetId,
        id: String,
        content_hash: String,
        validation: Validation,
    },
    EditorClosed {
        id: String,
        /// The editor worked on a temporary copy (git source): nothing to rescan.
        copy: bool,
        result: Result<(), String>,
    },
    /// bpftrace was spawned for `Cmd::StartRun`.
    RunStarted {
        run_id: u64,
        at: Instant,
    },
    /// Spawning failed.
    RunFailed {
        run_id: u64,
        reason: String,
    },
    /// Output, stderr and exit of a run, coalesced while the UI is behind (spec §6.5);
    /// `at` is when the executor sent the batch.
    Run {
        run_id: u64,
        at: Instant,
        batch: Batch,
    },
    /// 250 ms heartbeat while a run is active (elapsed time).
    Tick(Instant),
    /// Result of `Cmd::ExportRun`: the files written.
    Exported(Result<Vec<PathBuf>, String>),
    /// One connect check finished.
    ConnectCheck {
        attempt: u64,
        check: Check,
    },
    /// SSH needs a person (password, passphrase, host key): offer the terminal.
    ConnectNeedsAuth {
        attempt: u64,
        message: String,
    },
    /// The attempt ended; the failed check was reported, `reason` is anything else.
    ConnectFailed {
        attempt: u64,
        reason: Option<String>,
    },
    /// The target's SSH master is gone (checked every few seconds while connected).
    ConnectionLost {
        target: TargetId,
        reason: String,
    },
    /// All checks passed; the executor has the session and a validator for `target`.
    Connected {
        attempt: u64,
        target: TargetId,
        dest: Dest,
        info: RemoteInfo,
        host: SystemInfo,
        bpftrace: (BpftraceInfo, Strategy),
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// Resolve the source (clone/update git) and scan it.
    Load { input: String },
    /// Scan the already resolved source again (no git access).
    Rescan { source: ResolvedSource },
    /// Host facts + bpftrace capabilities; sets up the validator.
    DetectEnv { target: TargetId },
    Validate {
        target: TargetId,
        id: String,
        request: ValidationRequest,
    },
    /// Suspend the TUI and run `$EDITOR`. `copy`: edit a temporary copy (git sources).
    OpenEditor { id: String, path: PathBuf, copy: bool },
    /// Spawn bpftrace with this exact argv (built by `bpftrace::command::run_argv`).
    /// On a remote target `argv` names `script.bt`; `script` is the local file to copy.
    StartRun {
        target: TargetId,
        run_id: u64,
        argv: Vec<OsString>,
        script: PathBuf,
    },
    /// Graceful stop: SIGINT, then SIGTERM/SIGKILL on timeouts.
    StopRun { target: TargetId, run_id: u64 },
    /// Write `text` (see `model::export`) and a copy of the run's raw NDJSON to files.
    /// `host` is set for remote targets and goes into the file names.
    ExportRun {
        run_id: u64,
        script_id: String,
        host: Option<String>,
        text: String,
    },
    /// Open the SSH master for `target` and run the connect checks. `interactive`: open
    /// the master in the plain terminal first (TUI suspended), so SSH can ask questions.
    Connect {
        attempt: u64,
        target: TargetId,
        dest: Dest,
        sudo: SudoChoice,
        bpftrace: Option<String>,
        interactive: bool,
    },
    /// Abandon a connect attempt (and close its master if it got that far).
    CancelConnect { attempt: u64 },
    /// Stop the target's run (EOF on the host), drop its validator, close the master.
    Disconnect { target: TargetId },
}
