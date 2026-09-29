//! Fleet run comparison (docs/design-fleet.md, F4): every map of a run across its hosts.
//! Pure: built from the members' `Run`s on each frame.

use std::cmp::Ordering;
use std::time::Duration;

use super::hist;
use super::panels::{Panel, PanelData};
use super::run_state::Run;
use crate::bpftrace::json::Bucket;

/// A host whose value is above this many times the median of the others gets `◀`.
pub const OUTLIER_FACTOR: f64 = 3.0;
/// A snapshot older than the host's print interval plus this is shown as behind.
const STALE_GRACE: Duration = Duration::from_secs(2);

pub struct Member<'a> {
    pub label: &'a str,
    pub run: &'a Run,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostLine {
    pub label: String,
    pub state: String,
    pub active: bool,
    pub elapsed: Duration,
    pub errors: u64,
    pub dropped: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistRow {
    pub host: String,
    /// No snapshot of this map from the host (yet).
    pub missing: bool,
    pub count: u64,
    /// Bucket labels (`[64, 128)`) holding the 50th/90th/99th percentile.
    pub p50: Option<String>,
    pub p90: Option<String>,
    pub p99: Option<String>,
    /// Highest non-empty bucket.
    pub max: Option<String>,
    /// p50 / p99 above `OUTLIER_FACTOR` × the other hosts' median.
    pub p50_outlier: bool,
    pub p99_outlier: bool,
    pub stale: bool,
}

impl HistRow {
    pub fn outlier(&self) -> bool {
        self.p50_outlier || self.p99_outlier
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyRow {
    pub key: String,
    pub total: f64,
    /// Per host, in host order; `None`: the key is not in that host's snapshot.
    pub values: Vec<Option<f64>>,
    pub outliers: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValueRow {
    pub host: String,
    pub value: Option<String>,
    pub outlier: bool,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Section {
    Hist {
        name: String,
        /// A keyed hist (`@x[comm] = hist()`): all keys summed per host.
        keyed: bool,
        rows: Vec<HistRow>,
        /// Buckets summed over the hosts.
        merged: Vec<Bucket>,
    },
    Table {
        name: String,
        hosts: Vec<String>,
        rows: Vec<KeyRow>,
        /// Keys beyond `top`.
        more: usize,
        stale: Vec<bool>,
    },
    Values {
        name: String,
        rows: Vec<ValueRow>,
    },
    /// Stats and time series: shown per host only.
    Other {
        name: String,
        kind: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub hosts: Vec<HostLine>,
    pub sections: Vec<Section>,
}

/// How a key × host table is sorted: by the total, or by one host's column.
pub type SortColumn = Option<usize>;

pub fn compare(members: &[Member], sort: SortColumn, top: usize) -> Comparison {
    let hosts = members
        .iter()
        .map(|m| HostLine {
            label: m.label.to_string(),
            state: m.run.state_label(),
            active: m.run.is_active(),
            elapsed: m.run.elapsed,
            errors: m.run.errors,
            dropped: m.run.dropped,
        })
        .collect();
    // Map names in order of first appearance on any host.
    let mut names: Vec<&str> = Vec::new();
    for m in members {
        for p in &m.run.panels.list {
            if !names.contains(&p.name.as_str()) {
                names.push(&p.name);
            }
        }
    }
    let sections = names
        .into_iter()
        .map(|name| {
            let panels: Vec<Option<&Panel>> = members
                .iter()
                .map(|m| m.run.panels.list.iter().find(|p| p.name == name))
                .collect();
            let stale: Vec<bool> = members
                .iter()
                .zip(&panels)
                .map(|(m, p)| p.is_some_and(|p| is_stale(m.run, p)))
                .collect();
            let kind = panels
                .iter()
                .flatten()
                .next()
                .map_or(PanelKind::Other("?"), |p| kind(p));
            match kind {
                PanelKind::Hist => hist_section(name, members, &panels, &stale),
                PanelKind::Table => table_section(name, members, &panels, stale, sort, top),
                PanelKind::Value => value_section(name, members, &panels, &stale),
                PanelKind::Other(kind) => Section::Other {
                    name: name.to_string(),
                    kind,
                },
            }
        })
        .collect();
    Comparison { hosts, sections }
}

enum PanelKind {
    Hist,
    Table,
    Value,
    Other(&'static str),
}

fn kind(p: &Panel) -> PanelKind {
    match p.data {
        PanelData::Hist(_) => PanelKind::Hist,
        PanelData::Table { .. } => PanelKind::Table,
        PanelData::Value { .. } => PanelKind::Value,
        _ => PanelKind::Other(p.kind()),
    }
}

/// Still running, and no new snapshot for longer than its usual interval plus a grace.
fn is_stale(run: &Run, p: &Panel) -> bool {
    if !run.is_active() {
        return false;
    }
    let interval = if p.updates > 1 {
        p.updated_at / u32::try_from(p.updates).unwrap_or(u32::MAX)
    } else {
        Duration::from_secs(1)
    };
    run.elapsed.saturating_sub(p.updated_at) > interval + STALE_GRACE
}

/// Buckets of a hist panel, all keys summed for a keyed one.
fn buckets(p: &Panel) -> (Vec<Bucket>, bool) {
    let PanelData::Hist(series) = &p.data else {
        return (Vec::new(), false);
    };
    let keyed = series.iter().any(|(k, _)| k.is_some());
    (merge(series.iter().map(|(_, b)| b.as_slice())), keyed)
}

/// Sum buckets with the same bounds; underflow first, overflow last.
pub fn merge<'a>(sets: impl Iterator<Item = &'a [Bucket]>) -> Vec<Bucket> {
    let mut out: Vec<Bucket> = Vec::new();
    for set in sets {
        for b in set {
            match out.iter_mut().find(|o| o.min == b.min && o.max == b.max) {
                Some(o) => o.count += b.count,
                None => out.push(*b),
            }
        }
    }
    out.sort_by(|a, b| match (a.min, b.min) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
        (Some(x), Some(y)) => x.cmp(&y),
    });
    out
}

/// Index of the bucket holding the `q` quantile (0..1) of the counts.
fn quantile(buckets: &[Bucket], q: f64) -> Option<usize> {
    let total: u64 = buckets.iter().map(|b| b.count).sum();
    if total == 0 {
        return None;
    }
    let rank = ((total as f64) * q).ceil().max(1.0) as u64;
    let mut seen = 0;
    buckets.iter().position(|b| {
        seen += b.count;
        seen >= rank
    })
}

/// A number standing for a bucket (for outlier comparison): its lower bound, or the upper
/// one for the underflow bucket.
fn magnitude(b: &Bucket) -> f64 {
    b.min.or(b.max).map_or(0.0, |v| v as f64)
}

/// For each value, whether it is above `OUTLIER_FACTOR` × the median of the others.
fn outliers(values: &[Option<f64>]) -> Vec<bool> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let Some(v) = v else { return false };
            let mut others: Vec<f64> = values
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .filter_map(|(_, o)| *o)
                .collect();
            if others.is_empty() {
                return false;
            }
            others.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
            let n = others.len();
            let median = if n % 2 == 1 {
                others[n / 2]
            } else {
                (others[n / 2 - 1] + others[n / 2]) / 2.0
            };
            *v > median * OUTLIER_FACTOR && *v > median.max(0.0) && *v > 0.0
        })
        .collect()
}

fn hist_section(name: &str, members: &[Member], panels: &[Option<&Panel>], stale: &[bool]) -> Section {
    let mut keyed = false;
    let sets: Vec<Option<Vec<Bucket>>> = panels
        .iter()
        .map(|p| {
            p.map(|p| {
                let (b, k) = buckets(p);
                keyed |= k;
                b
            })
        })
        .collect();
    let merged = merge(sets.iter().flatten().map(Vec::as_slice));
    let at = |q: f64| -> Vec<Option<f64>> {
        sets.iter()
            .map(|s| s.as_ref().and_then(|b| quantile(b, q).map(|i| magnitude(&b[i]))))
            .collect()
    };
    let (p50_marks, p99_marks) = (outliers(&at(0.5)), outliers(&at(0.99)));
    let rows = members
        .iter()
        .zip(&sets)
        .enumerate()
        .map(|(i, (m, set))| {
            let empty = Vec::new();
            let b = set.as_ref().unwrap_or(&empty);
            let labels = hist::labels(b);
            let at = |q: f64| quantile(b, q).map(|i| labels[i].clone());
            HistRow {
                host: m.label.to_string(),
                missing: set.is_none(),
                count: b.iter().map(|b| b.count).sum(),
                p50: at(0.5),
                p90: at(0.9),
                p99: at(0.99),
                max: b.iter().rposition(|b| b.count > 0).map(|i| labels[i].clone()),
                p50_outlier: p50_marks[i],
                p99_outlier: p99_marks[i],
                stale: stale[i],
            }
        })
        .collect();
    Section::Hist {
        name: name.to_string(),
        keyed,
        rows,
        merged,
    }
}

fn table_section(
    name: &str,
    members: &[Member],
    panels: &[Option<&Panel>],
    stale: Vec<bool>,
    sort: SortColumn,
    top: usize,
) -> Section {
    let mut keys: Vec<&str> = Vec::new();
    for p in panels.iter().flatten() {
        if let PanelData::Table { rows, .. } = &p.data {
            for (k, _) in rows {
                if !keys.contains(&k.as_str()) {
                    keys.push(k);
                }
            }
        }
    }
    let value = |p: &Option<&Panel>, key: &str| -> Option<f64> {
        let PanelData::Table { rows, .. } = &(*p)?.data else {
            return None;
        };
        rows.iter().find(|(k, _)| k == key)?.1.as_f64()
    };
    let mut rows: Vec<KeyRow> = keys
        .into_iter()
        .map(|key| {
            let values: Vec<Option<f64>> = panels.iter().map(|p| value(p, key)).collect();
            KeyRow {
                key: key.to_string(),
                total: values.iter().flatten().sum(),
                outliers: outliers(&values),
                values,
            }
        })
        .collect();
    let by = |r: &KeyRow| match sort {
        Some(col) => r.values.get(col).copied().flatten().unwrap_or(f64::MIN),
        None => r.total,
    };
    rows.sort_by(|a, b| {
        by(b)
            .partial_cmp(&by(a))
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.key.cmp(&b.key))
    });
    let more = rows.len().saturating_sub(top);
    rows.truncate(top);
    Section::Table {
        name: name.to_string(),
        hosts: members.iter().map(|m| m.label.to_string()).collect(),
        rows,
        more,
        stale,
    }
}

fn value_section(name: &str, members: &[Member], panels: &[Option<&Panel>], stale: &[bool]) -> Section {
    let raw: Vec<Option<&serde_json::Value>> = panels
        .iter()
        .map(|p| match &(*p)?.data {
            PanelData::Value { value, .. } => Some(value),
            _ => None,
        })
        .collect();
    let marks = outliers(&raw.iter().map(|v| v.and_then(|v| v.as_f64())).collect::<Vec<_>>());
    let rows = members
        .iter()
        .zip(&raw)
        .enumerate()
        .map(|(i, (m, v))| ValueRow {
            host: m.label.to_string(),
            value: v.map(super::panels::display),
            outlier: marks[i],
            stale: stale[i],
        })
        .collect();
    Section::Values {
        name: name.to_string(),
        rows,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::bpftrace::json::parse_line;
    use pretty_assertions::assert_eq;

    fn run(lines: &[&str], secs: u64) -> Run {
        let mut run = Run::new(1, "x.bt", "bpftrace");
        let t0 = Instant::now();
        run.started(t0);
        for line in lines {
            for msg in parse_line(line) {
                run.output(msg, t0 + Duration::from_secs(secs));
            }
        }
        run.tick(t0 + Duration::from_secs(secs));
        run
    }

    fn hist_line(counts: &[(i64, i64, u64)]) -> String {
        let buckets: Vec<String> = counts
            .iter()
            .map(|(min, max, count)| format!("{{\"min\": {min}, \"max\": {max}, \"count\": {count}}}"))
            .collect();
        format!(
            "{{\"type\": \"hist\", \"data\": {{\"@usecs\": [{}]}}}}",
            buckets.join(", ")
        )
    }

    #[test]
    fn hist_percentiles_merge_and_outliers() {
        let fast = hist_line(&[(16, 31, 50), (32, 63, 45), (64, 127, 5)]);
        let slow = hist_line(&[(16, 31, 10), (32, 63, 10), (8192, 16383, 80)]);
        let (a, b, c) = (run(&[&fast], 3), run(&[&fast], 3), run(&[&slow], 3));
        let members = [
            Member {
                label: "db-01",
                run: &a,
            },
            Member {
                label: "db-02",
                run: &b,
            },
            Member {
                label: "db-03",
                run: &c,
            },
        ];
        let cmp = compare(&members, None, 10);
        let Section::Hist {
            rows, merged, keyed, ..
        } = &cmp.sections[0]
        else {
            panic!("{:?}", cmp.sections)
        };
        assert!(!keyed);
        let r = &rows[0];
        assert_eq!(
            (
                r.count,
                r.p50.as_deref(),
                r.p90.as_deref(),
                r.p99.as_deref(),
                r.max.as_deref()
            ),
            (
                100,
                Some("[16, 32)"),
                Some("[32, 64)"),
                Some("[64, 128)"),
                Some("[64, 128)")
            )
        );
        assert_eq!(rows[2].p99.as_deref(), Some("[8K, 16K)"));
        assert_eq!(
            rows.iter().map(|r| r.p99_outlier).collect::<Vec<_>>(),
            vec![false, false, true]
        );
        assert_eq!(
            rows.iter().map(|r| r.p50_outlier).collect::<Vec<_>>(),
            vec![false, false, true]
        );
        assert!(rows[2].outlier());
        let counts: Vec<(Option<i64>, u64)> = merged.iter().map(|b| (b.min, b.count)).collect();
        assert_eq!(
            counts,
            vec![(Some(16), 110), (Some(32), 100), (Some(64), 10), (Some(8192), 80)]
        );
        assert_eq!(cmp.hosts[2].label, "db-03");
    }

    #[test]
    fn tables_by_key_and_host() {
        let a = run(
            &[r#"{"type": "map", "data": {"@syscalls": {"postgres": 1207, "sshd": 101}}}"#],
            2,
        );
        let b = run(
            &[r#"{"type": "map", "data": {"@syscalls": {"postgres": 8205, "sshd": 103, "cron": 5}}}"#],
            2,
        );
        let c = run(
            &[r#"{"type": "map", "data": {"@syscalls": {"postgres": 1100, "sshd": 99}}}"#],
            2,
        );
        let members = [
            Member {
                label: "db-01",
                run: &a,
            },
            Member {
                label: "db-02",
                run: &b,
            },
            Member {
                label: "db-03",
                run: &c,
            },
        ];
        let Section::Table { rows, more, .. } = &compare(&members, None, 2).sections[0] else {
            panic!()
        };
        assert_eq!(*more, 1);
        assert_eq!(rows[0].key, "postgres");
        assert_eq!(rows[0].total, 10512.0);
        assert_eq!(rows[0].outliers, vec![false, true, false]);
        assert_eq!(rows[1].values, vec![Some(101.0), Some(103.0), Some(99.0)]);
        let Section::Table { rows, .. } = &compare(&members, Some(0), 3).sections[0] else {
            panic!()
        };
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            vec!["postgres", "sshd", "cron"],
            "by db-01's column; missing last"
        );
        assert_eq!(rows[2].values, vec![None, Some(5.0), None]);
    }

    #[test]
    fn values_missing_maps_and_staleness() {
        let a = run(&[r#"{"type": "map", "data": {"@total": 40}}"#], 1);
        let b = run(&[r#"{"type": "map", "data": {"@total": 50}}"#], 10);
        let members = [Member { label: "a", run: &a }, Member { label: "b", run: &b }];
        let Section::Values { rows, .. } = &compare(&members, None, 10).sections[0] else {
            panic!()
        };
        assert_eq!(rows[0].value.as_deref(), Some("40"));
        assert!(!rows[0].outlier && !rows[1].outlier);
        // b's only snapshot is at 10 s and b's run is at 10 s: fresh. a is at 1 s of 1 s.
        assert!(!rows[0].stale && !rows[1].stale);
        let mut late = run(&[r#"{"type": "map", "data": {"@total": 40}}"#], 1);
        late.elapsed = Duration::from_secs(9);
        let members = [
            Member {
                label: "a",
                run: &late,
            },
            Member { label: "b", run: &b },
        ];
        let Section::Values { rows, .. } = &compare(&members, None, 10).sections[0] else {
            panic!()
        };
        assert!(rows[0].stale, "no update for 8 s with a 1 s interval");

        // A map only one host printed: the others are missing.
        let h = run(&[&hist_line(&[(0, 0, 3)])], 1);
        let members = [Member { label: "a", run: &h }, Member { label: "b", run: &b }];
        let cmp = compare(&members, None, 10);
        let Section::Hist { rows, .. } = &cmp.sections[0] else {
            panic!()
        };
        assert!(rows[1].missing && rows[1].p50.is_none());
        assert_eq!(cmp.sections.len(), 2);
    }

    #[test]
    fn outlier_rule() {
        let o = |v: &[Option<f64>]| outliers(v);
        assert_eq!(o(&[Some(1.0), Some(1.0), Some(3.5)]), vec![false, false, true]);
        assert_eq!(
            o(&[Some(1.0), Some(3.0)]),
            vec![false, false],
            "exactly 3× is not above"
        );
        assert_eq!(o(&[Some(0.0), Some(5.0)]), vec![false, true]);
        assert_eq!(
            o(&[Some(5.0), None]),
            vec![false, false],
            "nothing to compare with"
        );
        assert_eq!(o(&[Some(0.0), Some(0.0)]), vec![false, false]);
    }
}
