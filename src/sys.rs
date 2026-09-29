//! Host facts: privileges, kernel lockdown, kernel release (spec §6.4, §7). Linux-first;
//! on other systems the probes degrade to "unknown"/"no privileges" instead of failing.

use std::fs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemInfo {
    pub privilege: Privilege,
    pub lockdown: Lockdown,
    pub kernel_release: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privilege {
    Root,
    /// Not root, but has the capabilities bpftrace needs.
    Caps,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lockdown {
    None,
    /// Blocks bpftrace (kprobes, BPF writes to user memory, …).
    Integrity,
    Confidentiality,
    /// No securityfs file or unrecognized content.
    Unknown,
}

impl Lockdown {
    pub fn blocks_bpftrace(self) -> bool {
        matches!(self, Self::Integrity | Self::Confidentiality)
    }
}

const CAP_SYS_ADMIN: u32 = 21;
const CAP_PERFMON: u32 = 38;
const CAP_BPF: u32 = 39;

pub fn detect() -> SystemInfo {
    let cap_eff = fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| parse_cap_eff(&s));
    SystemInfo {
        privilege: privilege(nix::unistd::geteuid().is_root(), cap_eff),
        lockdown: fs::read_to_string("/sys/kernel/security/lockdown")
            .map_or(Lockdown::Unknown, |s| parse_lockdown(&s)),
        kernel_release: fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string()),
    }
}

/// Root wins; otherwise CAP_BPF+CAP_PERFMON (5.8+) or CAP_SYS_ADMIN (older kernels).
pub(crate) fn privilege(is_root: bool, cap_eff: Option<u64>) -> Privilege {
    let has = |cap: u32| cap_eff.is_some_and(|c| c & (1 << cap) != 0);
    if is_root {
        Privilege::Root
    } else if has(CAP_SYS_ADMIN) || (has(CAP_BPF) && has(CAP_PERFMON)) {
        Privilege::Caps
    } else {
        Privilege::None
    }
}

/// `CapEff:\t000001ffffffffff` from /proc/self/status.
fn parse_cap_eff(status: &str) -> Option<u64> {
    let hex = status.lines().find_map(|l| l.strip_prefix("CapEff:"))?.trim();
    u64::from_str_radix(hex, 16).ok()
}

/// `none [integrity] confidentiality`: the bracketed word is the active mode.
pub(crate) fn parse_lockdown(content: &str) -> Lockdown {
    let active = content
        .split_whitespace()
        .find_map(|w| w.strip_prefix('[')?.strip_suffix(']'));
    match active {
        Some("none") => Lockdown::None,
        Some("integrity") => Lockdown::Integrity,
        Some("confidentiality") => Lockdown::Confidentiality,
        _ => Lockdown::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockdown() {
        assert_eq!(
            parse_lockdown("[none] integrity confidentiality\n"),
            Lockdown::None
        );
        assert_eq!(
            parse_lockdown("none [integrity] confidentiality\n"),
            Lockdown::Integrity
        );
        assert_eq!(
            parse_lockdown("none integrity [confidentiality]"),
            Lockdown::Confidentiality
        );
        assert_eq!(parse_lockdown(""), Lockdown::Unknown);
        assert_eq!(parse_lockdown("[weird]"), Lockdown::Unknown);
        assert!(Lockdown::Integrity.blocks_bpftrace());
        assert!(!Lockdown::Unknown.blocks_bpftrace());
    }

    #[test]
    fn cap_eff() {
        let status = "Name:\tbash\nCapInh:\t0000000000000000\nCapEff:\t000001ffffffffff\nCapBnd:\t0\n";
        assert_eq!(parse_cap_eff(status), Some(0x1ff_ffff_ffff));
        assert_eq!(parse_cap_eff("Name: x\n"), None);
        assert_eq!(parse_cap_eff("CapEff:\tzz\n"), None);
    }

    #[test]
    fn privileges() {
        let bit = |c: u32| 1u64 << c;
        assert_eq!(privilege(true, None), Privilege::Root);
        assert_eq!(privilege(false, Some(0)), Privilege::None);
        assert_eq!(privilege(false, None), Privilege::None);
        assert_eq!(privilege(false, Some(bit(CAP_SYS_ADMIN))), Privilege::Caps);
        assert_eq!(
            privilege(false, Some(bit(CAP_BPF) | bit(CAP_PERFMON))),
            Privilege::Caps
        );
        assert_eq!(privilege(false, Some(bit(CAP_BPF))), Privilege::None);
    }

    #[test]
    fn detect_never_fails() {
        let info = detect();
        assert!(!info.kernel_release.is_empty());
    }
}
