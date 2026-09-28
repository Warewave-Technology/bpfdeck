//! Panels (spec §5.4): one per map name, created on first appearance. Each new message
//! for a name replaces its snapshot; tables keep the previous one for ↑/↓ markers.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use crate::bpftrace::json::{Bucket, HistSeries, MapValue, OutputMsg};

#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub interval_start: String,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PanelData {
    /// `(key, buckets)`; key is `None` for a plain `@x = hist()`.
    Hist(Vec<(Option<String>, Vec<Bucket>)>),
    Table {
        rows: Vec<(String, Value)>,
        /// The snapshot before this one (for deltas); `None` until the second print.
        prev: Option<HashMap<String, Value>>,
    },
    /// `@x = 5`: big value; `changed_at` is run time of the last actual change.
    Value {
        value: Value,
        changed_at: Duration,
    },
    /// `stats` messages and anything else rendered as a generic table.
    Stats(Value),
    Tseries(Vec<(Option<String>, Vec<Point>)>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    pub name: String,
    pub data: PanelData,
    pub updates: u64,
    /// Run time of the latest message.
    pub updated_at: Duration,
    /// Selected key of a keyed hist/tseries (by name, so it survives updates).
    pub key: Option<String>,
    /// Tables: sort by key instead of by value (`s`).
    pub sort_by_key: bool,
}

impl Panel {
    pub fn kind(&self) -> &'static str {
        match self.data {
            PanelData::Hist(_) => "hist",
            PanelData::Table { .. } => "table",
            PanelData::Value { .. } => "value",
            PanelData::Stats(_) => "stats",
            PanelData::Tseries(_) => "tseries",
        }
    }

    /// Keys of a keyed hist/tseries, in bpftrace order; empty otherwise.
    pub fn keys(&self) -> Vec<&str> {
        match &self.data {
            PanelData::Hist(series) => series.iter().filter_map(|(k, _)| k.as_deref()).collect(),
            PanelData::Tseries(series) => series.iter().filter_map(|(k, _)| k.as_deref()).collect(),
            _ => Vec::new(),
        }
    }

    /// Index of the selected key (the first one if unset or gone).
    pub fn key_index(&self) -> usize {
        let keys = self.keys();
        self.key
            .as_deref()
            .and_then(|k| keys.iter().position(|x| *x == k))
            .unwrap_or(0)
    }

    pub fn cycle_key(&mut self, delta: isize) {
        let keys = self.keys();
        if keys.is_empty() {
            return;
        }
        let n = keys.len() as isize;
        let i = (self.key_index() as isize + delta).rem_euclid(n) as usize;
        self.key = Some(keys[i].to_string());
    }

    pub fn selected_hist(&self) -> Option<&[Bucket]> {
        let PanelData::Hist(series) = &self.data else {
            return None;
        };
        series.get(self.key_index()).map(|(_, b)| b.as_slice())
    }

    pub fn selected_tseries(&self) -> Option<&[Point]> {
        let PanelData::Tseries(series) = &self.data else {
            return None;
        };
        series.get(self.key_index()).map(|(_, p)| p.as_slice())
    }
}

#[derive(Debug, Clone, Default)]
pub struct Panels {
    /// In order of first appearance.
    pub list: Vec<Panel>,
    pub focus: usize,
    /// The user picked a panel (Tab): never move the focus on their behalf.
    user_focused: bool,
    /// Index of the panel that received the latest message.
    last_touched: usize,
}

impl Panels {
    /// Take in a snapshot message. Returns `false` for messages that are not panel data.
    pub fn apply(&mut self, msg: &OutputMsg, now: Duration) -> bool {
        let (name, data) = match msg {
            OutputMsg::Map { name, value } => (name, self.map_data(name, value, now)),
            OutputMsg::Hist { name, series } => (name, PanelData::Hist(hist_series(series))),
            OutputMsg::Stats { name, value } => (name, PanelData::Stats(value.clone())),
            OutputMsg::Tseries { name, value } => (
                name,
                tseries(value).map_or_else(|| PanelData::Stats(value.clone()), PanelData::Tseries),
            ),
            _ => return false,
        };
        match self.list.iter().position(|p| &p.name == name) {
            Some(i) => {
                let panel = &mut self.list[i];
                panel.data = data;
                panel.updates += 1;
                panel.updated_at = now;
                self.last_touched = i;
            }
            None => {
                self.last_touched = self.list.len();
                self.list.push(Panel {
                    name: name.clone(),
                    data,
                    updates: 1,
                    updated_at: now,
                    key: None,
                    sort_by_key: false,
                })
            }
        }
        true
    }

    /// At exit: show what the exit-time dump updated last (e.g. biolatency's histogram),
    /// unless the user already chose a panel.
    pub fn focus_final(&mut self) {
        if !self.user_focused && self.last_touched < self.list.len() {
            self.focus = self.last_touched;
        }
    }

    fn map_data(&self, name: &str, value: &MapValue, now: Duration) -> PanelData {
        let old = self.list.iter().find(|p| p.name == name).map(|p| &p.data);
        match value {
            MapValue::Keyed(rows) => PanelData::Table {
                rows: rows.clone(),
                prev: match old {
                    Some(PanelData::Table { rows, .. }) => Some(rows.iter().cloned().collect()),
                    _ => None,
                },
            },
            MapValue::Scalar(v) => {
                let changed_at = match old {
                    Some(PanelData::Value { value, changed_at }) if value == v => *changed_at,
                    _ => now,
                };
                PanelData::Value {
                    value: v.clone(),
                    changed_at,
                }
            }
        }
    }

    pub fn focused(&self) -> Option<&Panel> {
        self.list.get(self.focus)
    }

    pub fn focused_mut(&mut self) -> Option<&mut Panel> {
        self.list.get_mut(self.focus)
    }

    pub fn cycle_focus(&mut self, delta: isize) {
        if !self.list.is_empty() {
            self.user_focused = true;
            let n = self.list.len() as isize;
            self.focus = (self.focus as isize + delta).rem_euclid(n) as usize;
        }
    }
}

fn hist_series(series: &HistSeries) -> Vec<(Option<String>, Vec<Bucket>)> {
    match series {
        HistSeries::Single(b) => vec![(None, b.clone())],
        HistSeries::Keyed(keyed) => keyed.iter().map(|(k, b)| (Some(k.clone()), b.clone())).collect(),
    }
}

/// `[{interval_start, value}, …]` or `{key: [...], …}`; `None` for any other shape.
fn tseries(value: &Value) -> Option<Vec<(Option<String>, Vec<Point>)>> {
    fn points(v: &Value) -> Option<Vec<Point>> {
        v.as_array()?
            .iter()
            .map(|p| {
                Some(Point {
                    interval_start: p.get("interval_start")?.as_str().unwrap_or_default().to_string(),
                    value: p.get("value")?.as_f64()?,
                })
            })
            .collect()
    }
    match value {
        Value::Array(_) => Some(vec![(None, points(value)?)]),
        Value::Object(keyed) => keyed
            .iter()
            .map(|(k, v)| Some((Some(k.clone()), points(v)?)))
            .collect(),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delta {
    /// First snapshot: nothing to compare with.
    None,
    New,
    Up,
    Down,
    Same,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableRow {
    /// Key split into tuple columns.
    pub cols: Vec<String>,
    pub value: String,
    pub num: Option<f64>,
    pub delta: Delta,
}

/// Rows for a top table: sorted by value (desc) or by key, with deltas vs `prev`.
pub fn table_rows(
    rows: &[(String, Value)],
    prev: Option<&HashMap<String, Value>>,
    by_key: bool,
) -> Vec<TableRow> {
    let mut out: Vec<(String, TableRow)> = rows
        .iter()
        .map(|(key, value)| {
            let num = value.as_f64();
            let delta = match prev {
                None => Delta::None,
                Some(prev) => match (prev.get(key), num) {
                    (None, _) => Delta::New,
                    (Some(old), Some(n)) => match old.as_f64().map(|o| n.partial_cmp(&o)) {
                        Some(Some(Ordering::Greater)) => Delta::Up,
                        Some(Some(Ordering::Less)) => Delta::Down,
                        _ => Delta::Same,
                    },
                    (Some(_), None) => Delta::Same,
                },
            };
            let row = TableRow {
                cols: key.split(',').map(str::to_string).collect(),
                value: display(value),
                num,
                delta,
            };
            (key.clone(), row)
        })
        .collect();
    if by_key {
        out.sort_by(|(a, _), (b, _)| key_order(a, b));
    } else {
        out.sort_by(|(ka, a), (kb, b)| match (a.num, b.num) {
            (Some(x), Some(y)) => y
                .partial_cmp(&x)
                .unwrap_or(Ordering::Equal)
                .then_with(|| key_order(ka, kb)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => key_order(ka, kb),
        });
    }
    out.into_iter().map(|(_, r)| r).collect()
}

/// Numeric keys (pids, cpus) in numeric order, everything else lexically.
fn key_order(a: &str, b: &str) -> Ordering {
    match (a.parse::<i64>(), b.parse::<i64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => a.cmp(b),
    }
}

/// Strings without quotes, numbers as printed, anything else as compact JSON.
pub fn display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Generic table for `stats` and unknown shapes: `(headers, rows)`.
pub fn stats_table(v: &Value) -> (Vec<String>, Vec<Vec<String>>) {
    const FIELDS: [&str; 3] = ["count", "average", "total"];
    let is_stats = |o: &Value| FIELDS.iter().all(|f| o.get(f).is_some());
    let fields = |o: &Value| {
        FIELDS
            .iter()
            .map(|f| o.get(f).map(display).unwrap_or_default())
            .collect::<Vec<_>>()
    };
    let headers = |first: &[&str]| first.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match v {
        o if is_stats(o) => (headers(&FIELDS), vec![fields(o)]),
        Value::Object(map) if !map.is_empty() && map.values().all(is_stats) => (
            headers(&["key", "count", "average", "total"]),
            map.iter()
                .map(|(k, o)| std::iter::once(k.clone()).chain(fields(o)).collect())
                .collect(),
        ),
        Value::Object(map) => (
            headers(&["key", "value"]),
            map.iter().map(|(k, v)| vec![k.clone(), display(v)]).collect(),
        ),
        other => (headers(&["value"]), vec![vec![display(other)]]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpftrace::json::parse_line;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn apply(panels: &mut Panels, line: &str, secs: u64) {
        for msg in parse_line(line) {
            assert!(panels.apply(&msg, Duration::from_secs(secs)), "{line}");
        }
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/json/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture")
    }

    #[test]
    fn panels_are_created_in_order_and_replaced() {
        let mut p = Panels::default();
        for line in fixture("session_mixed.ndjson").lines() {
            for msg in parse_line(line) {
                p.apply(&msg, Duration::ZERO);
            }
        }
        let names: Vec<_> = p
            .list
            .iter()
            .map(|p| (p.name.as_str(), p.kind(), p.updates))
            .collect();
        assert_eq!(names, vec![("@syscalls", "table", 2), ("@usecs", "hist", 1)]);
        assert!(!p.apply(&OutputMsg::AttachedProbes(1), Duration::ZERO));
        assert!(!p.apply(&parse_line(&fixture("printf.ndjson")).remove(0), Duration::ZERO));
    }

    #[test]
    fn table_deltas_sorting_and_tuple_keys() {
        let mut p = Panels::default();
        apply(
            &mut p,
            r#"{"type":"map","data":{"@m":{"bash":12,"sshd":40,"node":311}}}"#,
            1,
        );
        let PanelData::Table { rows, prev } = &p.list[0].data else {
            panic!()
        };
        let first = table_rows(rows, prev.as_ref(), false);
        assert_eq!(
            first.iter().map(|r| r.cols[0].as_str()).collect::<Vec<_>>(),
            vec!["node", "sshd", "bash"]
        );
        assert!(first.iter().all(|r| r.delta == Delta::None));

        apply(
            &mut p,
            r#"{"type":"map","data":{"@m":{"bash":3,"sshd":55,"node":311,"curl":7}}}"#,
            2,
        );
        let PanelData::Table { rows, prev } = &p.list[0].data else {
            panic!()
        };
        let rows_v = table_rows(rows, prev.as_ref(), false);
        let got: Vec<_> = rows_v
            .iter()
            .map(|r| (r.cols[0].as_str(), r.value.as_str(), r.delta))
            .collect();
        assert_eq!(
            got,
            vec![
                ("node", "311", Delta::Same),
                ("sshd", "55", Delta::Up),
                ("curl", "7", Delta::New),
                ("bash", "3", Delta::Down),
            ]
        );
        let by_key: Vec<_> = table_rows(rows, prev.as_ref(), true)
            .into_iter()
            .map(|r| r.cols[0].clone())
            .collect();
        assert_eq!(by_key, vec!["bash", "curl", "node", "sshd"]);

        let tuples = table_rows(
            &[
                ("bpftrace,2".into(), json!(5)),
                ("10".into(), json!(5)),
                ("9".into(), json!(5)),
            ],
            None,
            false,
        );
        assert_eq!(tuples[0].cols, vec!["9"], "ties sort by key, numerically");
        assert_eq!(tuples[2].cols, vec!["bpftrace", "2"]);
    }

    #[test]
    fn scalar_values_track_last_change() {
        let mut p = Panels::default();
        apply(&mut p, r#"{"type":"map","data":{"@x":5}}"#, 1);
        apply(&mut p, r#"{"type":"map","data":{"@x":5}}"#, 2);
        let PanelData::Value { changed_at, .. } = &p.list[0].data else {
            panic!()
        };
        assert_eq!(*changed_at, Duration::from_secs(1));
        apply(&mut p, r#"{"type":"map","data":{"@x":6}}"#, 3);
        let PanelData::Value { value, changed_at } = &p.list[0].data else {
            panic!()
        };
        assert_eq!((value, *changed_at), (&json!(6), Duration::from_secs(3)));
        assert_eq!(p.list[0].updates, 3);
        apply(&mut p, &fixture("scalar_str.ndjson"), 4);
        assert_eq!(p.list[1].kind(), "value");
    }

    #[test]
    fn keyed_hist_selection_survives_updates() {
        let mut p = Panels::default();
        apply(&mut p, &fixture("hist_multiple.ndjson"), 1);
        let panel = &mut p.list[0];
        assert_eq!(panel.keys(), vec!["bpftrace", "curl"]);
        assert_eq!(panel.selected_hist().map(<[Bucket]>::len), Some(1));
        panel.cycle_key(1);
        assert_eq!(panel.key.as_deref(), Some("curl"));
        panel.cycle_key(1);
        assert_eq!(panel.key.as_deref(), Some("bpftrace"), "wraps");
        panel.cycle_key(-1);
        // New snapshot with the keys reordered: the selection follows the name.
        apply(
            &mut p,
            r#"{"type":"hist","data":{"@":{"curl":[{"min":0,"max":0,"count":1}],"x":[{"min":0,"max":0,"count":1}]}}}"#,
            2,
        );
        assert_eq!(p.list[0].key_index(), 0);
        assert_eq!(p.list[0].keys()[0], "curl");
    }

    #[test]
    fn tseries_and_stats() {
        let mut p = Panels::default();
        apply(&mut p, &fixture("tseries.ndjson"), 1);
        let points = p.list[0].selected_tseries().expect("points");
        assert_eq!(points.len(), 5);
        assert_eq!(points[0].interval_start, "1970-01-01 00:00:00.500000000");
        assert_eq!(points[4].value, 9.0);

        apply(&mut p, r#"{"type":"tseries","data":{"@odd":{"shape":true}}}"#, 1);
        assert_eq!(
            p.list[1].kind(),
            "stats",
            "unknown shapes fall back to a generic table"
        );

        assert_eq!(
            stats_table(&json!({"count": 2, "average": 6, "total": 12})),
            (
                vec!["count".into(), "average".into(), "total".into()],
                vec![vec!["2".into(), "6".into(), "12".into()]]
            )
        );
        let (h, rows) = stats_table(
            &json!({"a": {"count": 1, "average": 2, "total": 2}, "b": {"count": 3, "average": 1, "total": 3}}),
        );
        assert_eq!(h[0], "key");
        assert_eq!(rows[1], vec!["b", "3", "1", "3"]);
        assert_eq!(
            stats_table(&json!({"c": 3, "d": 4})).1,
            vec![vec!["c", "3"], vec!["d", "4"]]
        );
        assert_eq!(stats_table(&json!([1, 2])).1, vec![vec!["[1,2]"]]);
    }

    #[test]
    fn exit_focuses_the_final_dump_unless_the_user_chose() {
        let mut p = panels_from(&fixture("session_mixed.ndjson"));
        apply(&mut p, r#"{"type":"map","data":{"@syscalls":{"a":1}}}"#, 9);
        apply(
            &mut p,
            r#"{"type":"hist","data":{"@usecs":[{"min":0,"max":0,"count":1}]}}"#,
            10,
        );
        assert_eq!(p.focus, 0);
        p.focus_final();
        assert_eq!(p.focused().map(|x| x.name.as_str()), Some("@usecs"));

        let mut p = panels_from(&fixture("session_mixed.ndjson"));
        p.cycle_focus(1);
        p.cycle_focus(1);
        apply(
            &mut p,
            r#"{"type":"hist","data":{"@usecs":[{"min":0,"max":0,"count":1}]}}"#,
            10,
        );
        p.focus_final();
        assert_eq!(p.focus, 0, "the user's choice wins");
    }

    fn panels_from(ndjson: &str) -> Panels {
        let mut p = Panels::default();
        for msg in ndjson.lines().flat_map(parse_line) {
            p.apply(&msg, Duration::from_secs(1));
        }
        p
    }

    #[test]
    fn focus_cycles() {
        let mut p = Panels::default();
        p.cycle_focus(1);
        assert_eq!(p.focus, 0);
        for line in fixture("multiple_maps.ndjson").lines() {
            apply(&mut p, line, 0);
        }
        assert_eq!(p.list.len(), 2);
        p.cycle_focus(1);
        assert_eq!(p.focused().map(|p| p.name.as_str()), Some("@map2"));
        p.cycle_focus(1);
        assert_eq!(p.focus, 0);
        p.cycle_focus(-1);
        assert_eq!(p.focus, 1);
    }
}
