//! Remote targets over SSH, agentless (D-020, docs/design-remote.md): nothing is installed
//! on the host; a fixed POSIX sh runner is sent over `ssh host sh -s` per operation.

pub mod connect;
pub mod facts;
pub mod session;

/// Name of the script inside the runner's temp dir; bpftrace runs there (runner.sh).
pub const REMOTE_SCRIPT: &str = "script.bt";

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
    #[error("invalid range {0:?} (use {{1..4}} or {{01..12}})")]
    InvalidRange(String),
    #[error("{0} hosts; at most {MAX_HOSTS} at once")]
    TooMany(usize),
}

/// Most hosts one connect dialog takes (docs/design-fleet.md: 2–20 targets).
pub const MAX_HOSTS: usize = 20;

/// The host field as a list: hosts separated by spaces or commas, each with optional
/// numeric ranges, `db-0{1..3}` → `db-01 db-02 db-03`, `10.0.3.{9..11}`. Zero padding of
/// the start is kept (`{08..10}` → `08 09 10`). Duplicates are dropped, order is kept.
/// No shell is involved; anything else in braces is an error.
pub fn expand_hosts(input: &str) -> Result<Vec<String>, DestError> {
    let mut hosts: Vec<String> = Vec::new();
    for token in input
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        for host in expand_token(token)? {
            if !hosts.contains(&host) {
                hosts.push(host);
            }
            if hosts.len() > MAX_HOSTS {
                return Err(DestError::TooMany(count_hosts(input)));
            }
        }
    }
    if hosts.is_empty() {
        return Err(DestError::Empty);
    }
    Ok(hosts)
}

/// How many hosts `input` names, for the error message (0 if it does not parse).
fn count_hosts(input: &str) -> usize {
    input
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(|t| expand_token(t).map_or(0, |h| h.len()))
        .sum()
}

fn expand_token(token: &str) -> Result<Vec<String>, DestError> {
    let Some(open) = token.find('{') else {
        if token.contains('}') {
            return Err(DestError::InvalidRange(token.to_string()));
        }
        return Ok(vec![token.to_string()]);
    };
    let bad = || DestError::InvalidRange(token.to_string());
    let close = token[open..].find('}').ok_or_else(bad)? + open;
    let (from, to) = token[open + 1..close].split_once("..").ok_or_else(bad)?;
    let (a, b): (u32, u32) = (from.parse().map_err(|_| bad())?, to.parse().map_err(|_| bad())?);
    if a > b || (b - a) as usize >= MAX_HOSTS * 10 {
        return Err(bad());
    }
    let width = if from.starts_with('0') { from.len() } else { 0 };
    let (head, tail) = (&token[..open], &token[close + 1..]);
    let mut out = Vec::new();
    for n in a..=b {
        for rest in expand_token(tail)? {
            out.push(format!("{head}{n:0width$}{rest}"));
        }
    }
    Ok(out)
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
    fn expands_host_lists() {
        let e = |s: &str| expand_hosts(s);
        assert_eq!(e("db-01"), Ok(vec!["db-01".into()]));
        assert_eq!(
            e(" ops@db-0{1..3}, 10.0.3.{9..10}  db-02"),
            Ok(vec![
                "ops@db-01".into(),
                "ops@db-02".into(),
                "ops@db-03".into(),
                "10.0.3.9".into(),
                "10.0.3.10".into(),
                "db-02".into(),
            ])
        );
        assert_eq!(e("h{08..10}"), Ok(vec!["h08".into(), "h09".into(), "h10".into()]));
        assert_eq!(e("r{1..2}-n{1..2}").map(|h| h.len()), Ok(4));
        assert_eq!(e("a a,a"), Ok(vec!["a".into()]), "duplicates dropped");
        assert_eq!(e(" , "), Err(DestError::Empty));
        for bad in ["h{3..1}", "h{a..b}", "h{1,2}", "h{1..", "h}", "h{1..99999}"] {
            assert!(matches!(e(bad), Err(DestError::InvalidRange(_))), "{bad}");
        }
        assert_eq!(e("h{1..21}"), Err(DestError::TooMany(21)));
        assert_eq!(e("h{1..20}").map(|h| h.len()), Ok(20));
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
