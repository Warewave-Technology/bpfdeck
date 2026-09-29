//! Targets: where scripts are validated and run. `local` always exists; SSH hosts are
//! added from the connect dialog. Each target has its own results tab.

use super::{BpftraceState, LogView};
use crate::model::run_state::Run;
use crate::remote::Dest;
use crate::sys::SystemInfo;

pub type TargetId = u32;
/// The machine bpfdeck runs on.
pub const LOCAL: TargetId = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetKind {
    Local,
    Ssh(Dest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conn {
    Ready,
    /// The SSH connection went away; runs there were stopped by the runner (EOF).
    Lost(String),
}

/// Facts about a remote host from the connect checks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteInfo {
    pub os: String,
    pub arch: String,
    /// `root`, `root via sudo`, `root via sudo (password)`.
    pub privilege: String,
}

#[derive(Debug)]
pub struct Target {
    pub id: TargetId,
    pub label: String,
    pub kind: TargetKind,
    pub conn: Conn,
    pub host: Option<SystemInfo>,
    pub remote: Option<RemoteInfo>,
    pub bpftrace: BpftraceState,
    /// The current or last run on this target (one at a time per target, D-010).
    pub run: Option<Run>,
    pub log_view: LogView,
}

impl Target {
    pub fn local() -> Self {
        Self::new(LOCAL, "local".into(), TargetKind::Local)
    }

    pub fn new(id: TargetId, label: String, kind: TargetKind) -> Self {
        Self {
            id,
            label,
            kind,
            conn: Conn::Ready,
            host: None,
            remote: None,
            bpftrace: BpftraceState::Detecting,
            run: None,
            log_view: LogView::following(),
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self.kind, TargetKind::Ssh(_))
    }

    pub fn active_run(&self) -> Option<&Run> {
        self.run.as_ref().filter(|r| r.is_active())
    }

    /// Validation and runs can go out: connected and bpftrace found.
    pub fn usable(&self) -> bool {
        self.conn == Conn::Ready && matches!(self.bpftrace, BpftraceState::Ready { .. })
    }
}
