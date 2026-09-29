//! Remote targets over SSH, agentless (D-020, docs/design-remote.md): nothing is installed
//! on the host; a fixed POSIX sh runner is sent over `ssh host sh -s` per operation.

/// Where to connect, as typed in the connect dialog. Passed to `ssh` as separate argv
/// entries after `--`, never through a shell.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Dest {
    pub user: Option<String>,
    /// IP, hostname or `~/.ssh/config` alias.
    pub host: String,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DestError {
    #[error("enter a host (IP, hostname, user@host or an ssh config alias)")]
    Empty,
    #[error("invalid host {0:?}")]
    InvalidHost(String),
    #[error("invalid user {0:?}")]
    InvalidUser(String),
    #[error("invalid port {0:?}")]
    InvalidPort(String),
}

impl Dest {
    /// `host`, `user@host`; `port` is a separate field (empty = ssh's default/config).
    pub fn parse(host: &str, port: &str) -> Result<Self, DestError> {
        let host = host.trim();
        if host.is_empty() {
            return Err(DestError::Empty);
        }
        let (user, host) = match host.rsplit_once('@') {
            Some((user, host)) => (Some(user.to_string()), host.to_string()),
            None => (None, host.to_string()),
        };
        // Anything that ssh could read as an option, or that is not a plausible name/address.
        let plain = |s: &str| {
            !s.is_empty()
                && !s.starts_with('-')
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._-:[]%".contains(c))
        };
        if !plain(&host) {
            return Err(DestError::InvalidHost(host));
        }
        if let Some(u) = &user
            && !(plain(u) && !u.contains(':'))
        {
            return Err(DestError::InvalidUser(u.clone()));
        }
        let port = match port.trim() {
            "" => None,
            p => Some(
                p.parse::<u16>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| DestError::InvalidPort(p.to_string()))?,
            ),
        };
        Ok(Self { user, host, port })
    }

    /// Tab label: what the user typed, `:port` when not the default.
    pub fn label(&self) -> String {
        let mut s = match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        };
        if let Some(p) = self.port {
            s.push_str(&format!(":{p}"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_hosts() {
        assert_eq!(
            Dest::parse(" 10.0.3.14 ", ""),
            Ok(Dest {
                user: None,
                host: "10.0.3.14".into(),
                port: None
            })
        );
        assert_eq!(
            Dest::parse("ops@db-02.example.com", "2222"),
            Ok(Dest {
                user: Some("ops".into()),
                host: "db-02.example.com".into(),
                port: Some(2222)
            })
        );
        assert_eq!(
            Dest::parse("fe80::1%eth0", "").map(|d| d.host),
            Ok("fe80::1%eth0".into())
        );
        assert_eq!(
            Dest::parse("ops@db-02", "2222").map(|d| d.label()),
            Ok("ops@db-02:2222".into())
        );
    }

    #[test]
    fn rejects_option_like_and_odd_input() {
        assert_eq!(Dest::parse("", ""), Err(DestError::Empty));
        for bad in ["-oProxyCommand=touch x", "host name", "a;b", "$(x)", "h\nx"] {
            assert!(
                matches!(Dest::parse(bad, ""), Err(DestError::InvalidHost(_))),
                "{bad:?}"
            );
        }
        assert!(matches!(
            Dest::parse("-l@host", ""),
            Err(DestError::InvalidUser(_))
        ));
        for bad in ["0", "70000", "x", "-1"] {
            assert!(
                matches!(Dest::parse("h", bad), Err(DestError::InvalidPort(_))),
                "{bad:?}"
            );
        }
    }
}
