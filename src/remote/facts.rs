//! What the connect checks learn about a host: a fixed sh program (no user data in it)
//! prints `key=value` lines, parsed here.

use crate::sys::{self, Lockdown, SystemInfo};

/// Sent as the session's script and run as `sh script.bt` (argv lines cannot hold a
/// multi-line program).
pub const FACTS_SCRIPT: &str = r#"echo "user=$(id -un 2>/dev/null)"
echo "uid=$(id -u)"
echo "os=$( (. /etc/os-release && echo "$PRETTY_NAME") 2>/dev/null)"
echo "arch=$(uname -m)"
echo "kernel=$(uname -r)"
echo "lockdown=$(cat /sys/kernel/security/lockdown 2>/dev/null)"
echo "capeff=$(sed -n 's/^CapEff:[[:space:]]*//p' /proc/self/status 2>/dev/null)"
[ -e /sys/kernel/btf/vmlinux ] && echo "btf=yes"
for t in mktemp head setsid; do command -v "$t" > /dev/null 2>&1 && echo "tool=$t"; done
echo "bpftrace=$(command -v bpftrace 2>/dev/null)"
"#;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    pub user: String,
    pub uid: Option<u32>,
    pub os: String,
    pub arch: String,
    pub kernel: String,
    pub lockdown: String,
    pub cap_eff: Option<u64>,
    pub btf: bool,
    pub tools: Vec<String>,
    /// `command -v bpftrace` in that session's PATH (empty: not found).
    pub bpftrace: String,
}

/// Facts about a remote host for its tab and header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteInfo {
    /// Login user.
    pub user: String,
    pub os: String,
    pub arch: String,
    /// `root`, `root via sudo`, `root via sudo (password)`.
    pub privilege: String,
    pub btf: bool,
}

impl Facts {
    pub fn parse(output: &str) -> Self {
        let mut f = Facts::default();
        for line in output.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().to_string();
            match key {
                "user" => f.user = value,
                "uid" => f.uid = value.parse().ok(),
                "os" => f.os = value,
                "arch" => f.arch = value,
                "kernel" => f.kernel = value,
                "lockdown" => f.lockdown = value,
                "capeff" => f.cap_eff = u64::from_str_radix(&value, 16).ok(),
                "btf" => f.btf = value == "yes",
                "tool" => f.tools.push(value),
                "bpftrace" => f.bpftrace = value,
                _ => {}
            }
        }
        f
    }

    pub fn is_root(&self) -> bool {
        self.uid == Some(0)
    }

    pub fn has(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t == tool)
    }

    /// The same facts `sys::detect` gathers locally.
    pub fn system_info(&self) -> SystemInfo {
        SystemInfo {
            privilege: sys::privilege(self.is_root(), self.cap_eff),
            lockdown: if self.lockdown.is_empty() {
                Lockdown::Unknown
            } else {
                sys::parse_lockdown(&self.lockdown)
            },
            kernel_release: if self.kernel.is_empty() {
                "unknown".into()
            } else {
                self.kernel.clone()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::Privilege;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_facts() {
        let out = "user=ops\nuid=0\nos=Rocky Linux 9.4 (Blue Onyx)\narch=x86_64\nkernel=5.14.0-427\n\
                   lockdown=[none] integrity confidentiality\ncapeff=000001ffffffffff\nbtf=yes\n\
                   tool=mktemp\ntool=head\nbpftrace=/usr/bin/bpftrace\nnoise without equals\n";
        let f = Facts::parse(out);
        assert_eq!(f.user, "ops");
        assert!(f.is_root() && f.btf && f.has("head") && !f.has("setsid"));
        assert_eq!(f.os, "Rocky Linux 9.4 (Blue Onyx)");
        assert_eq!(f.bpftrace, "/usr/bin/bpftrace");
        let sys = f.system_info();
        assert_eq!(sys.privilege, Privilege::Root);
        assert_eq!(sys.lockdown, Lockdown::None);
        assert_eq!(sys.kernel_release, "5.14.0-427");

        let empty = Facts::parse("uid=1000\nlockdown=\nbpftrace=\n");
        assert!(!empty.is_root());
        assert_eq!(empty.system_info().lockdown, Lockdown::Unknown);
        assert_eq!(empty.system_info().privilege, Privilege::None);
        assert!(empty.bpftrace.is_empty());
    }
}
