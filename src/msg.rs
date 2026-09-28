//! `Msg`: everything that happens to the app. `Cmd`: side effects the app asks for.
//! See docs/architecture.md ("Concurrency model").

use std::path::PathBuf;

use ratatui::crossterm::event::KeyEvent;

use crate::bpftrace::BpftraceInfo;
use crate::bpftrace::validate::{Strategy, Validation, ValidationRequest};
use crate::catalog::Catalog;
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
        host: SystemInfo,
        bpftrace: Result<(BpftraceInfo, Strategy), String>,
    },
    Validated {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cmd {
    /// Resolve the source (clone/update git) and scan it.
    Load {
        input: String,
    },
    /// Scan the already resolved source again (no git access).
    Rescan {
        source: ResolvedSource,
    },
    /// Host facts + bpftrace capabilities; sets up the validator.
    DetectEnv,
    Validate {
        id: String,
        request: ValidationRequest,
    },
    /// Suspend the TUI and run `$EDITOR`. `copy`: edit a temporary copy (git sources).
    OpenEditor {
        id: String,
        path: PathBuf,
        copy: bool,
    },
}
