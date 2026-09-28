//! Pure parser for `bpftrace -f json` NDJSON lines (docs/bpftrace-json.md, D-007).
//!
//! Every line maps to messages or is kept raw: unknown shapes are data, never errors.

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum OutputMsg {
    AttachedProbes(u64),
    Text {
        kind: TextKind,
        text: String,
    },
    /// `print(<non-map value>)`.
    Value(Value),
    Map {
        name: String,
        value: MapValue,
    },
    Hist {
        name: String,
        series: HistSeries,
    },
    Stats {
        name: String,
        value: Value,
    },
    Tseries {
        name: String,
        value: Value,
    },
    HelperError {
        msg: String,
        helper: String,
        line: Option<u32>,
    },
    /// Valid JSON we don't understand (unknown `type` or unexpected shape), kept raw.
    Unknown(Value),
    /// A stdout line that is not JSON (some versions leak warnings to stdout).
    NotJson(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Printf,
    Time,
    Cat,
    Join,
    Syscall,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MapValue {
    /// `@x = 5`, `@s = "str"`, tuples/arrays.
    Scalar(Value),
    /// Keyed map, in bpftrace's order. Integer and tuple keys arrive as strings.
    Keyed(Vec<(String, Value)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistSeries {
    Single(Vec<Bucket>),
    Keyed(Vec<(String, Vec<Bucket>)>),
}

/// One histogram bucket; `min` is missing on the underflow bucket, `max` on overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket {
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub count: u64,
}

/// Parse one stdout line. Blank lines yield nothing; a `data` object with several
/// `@name` keys yields one message per key.
pub fn parse_line(line: &str) -> Vec<OutputMsg> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(line) {
        Ok(value) => parse_value(value),
        Err(_) => vec![OutputMsg::NotJson(line.to_string())],
    }
}

fn parse_value(value: Value) -> Vec<OutputMsg> {
    let parsed = match value.get("type").and_then(Value::as_str) {
        Some("attached_probes") => attached_probes(&value).map(|n| vec![OutputMsg::AttachedProbes(n)]),
        Some("printf") => text(&value, TextKind::Printf),
        Some("time") => text(&value, TextKind::Time),
        Some("cat") => text(&value, TextKind::Cat),
        Some("join") => text(&value, TextKind::Join),
        Some("syscall") => text(&value, TextKind::Syscall),
        Some("value") => value.get("data").map(|d| vec![OutputMsg::Value(d.clone())]),
        Some("map") => named(&value, |name, v| {
            Some(OutputMsg::Map {
                name,
                value: map_value(v),
            })
        }),
        Some("hist") => named(&value, |name, v| {
            Some(OutputMsg::Hist {
                name,
                series: hist_series(v)?,
            })
        }),
        Some("stats") => named(&value, |name, v| {
            Some(OutputMsg::Stats {
                name,
                value: v.clone(),
            })
        }),
        Some("tseries") => named(&value, |name, v| {
            Some(OutputMsg::Tseries {
                name,
                value: v.clone(),
            })
        }),
        Some("helper_error") => helper_error(&value),
        _ => None,
    };
    parsed.unwrap_or_else(|| vec![OutputMsg::Unknown(value)])
}

/// `{"data": {"probes": N}}`, newer versions also a top-level `count`.
fn attached_probes(value: &Value) -> Option<u64> {
    value
        .get("data")
        .and_then(|d| d.get("probes"))
        .and_then(Value::as_u64)
        .or_else(|| value.get("count").and_then(Value::as_u64))
}

fn text(value: &Value, kind: TextKind) -> Option<Vec<OutputMsg>> {
    let text = value.get("data")?.as_str()?.to_string();
    Some(vec![OutputMsg::Text { kind, text }])
}

/// Apply `f` to every `@name` entry of `data`. Any entry that does not fit the expected
/// shape makes the whole line Unknown, so nothing is silently half-parsed.
fn named(value: &Value, f: impl Fn(String, &Value) -> Option<OutputMsg>) -> Option<Vec<OutputMsg>> {
    let data = value.get("data")?.as_object()?;
    if data.is_empty() {
        return None;
    }
    data.iter().map(|(name, v)| f(name.clone(), v)).collect()
}

fn map_value(v: &Value) -> MapValue {
    match v {
        Value::Object(entries) => MapValue::Keyed(entries_vec(entries)),
        other => MapValue::Scalar(other.clone()),
    }
}

fn entries_vec(entries: &Map<String, Value>) -> Vec<(String, Value)> {
    entries.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn hist_series(v: &Value) -> Option<HistSeries> {
    match v {
        Value::Array(buckets) => Some(HistSeries::Single(parse_buckets(buckets)?)),
        Value::Object(keyed) => keyed
            .iter()
            .map(|(k, b)| Some((k.clone(), parse_buckets(b.as_array()?)?)))
            .collect::<Option<Vec<_>>>()
            .map(HistSeries::Keyed),
        _ => None,
    }
}

fn parse_buckets(buckets: &[Value]) -> Option<Vec<Bucket>> {
    buckets
        .iter()
        .map(|b| {
            let b = b.as_object()?;
            let bound = |key| b.get(key).and_then(as_i64_saturating);
            let bucket = Bucket {
                min: bound("min"),
                max: bound("max"),
                count: b.get("count").and_then(Value::as_u64).unwrap_or(0),
            };
            // A bucket needs at least one bound to be placeable.
            (bucket.min.is_some() || bucket.max.is_some()).then_some(bucket)
        })
        .collect()
}

/// Bounds of the top log2 buckets can exceed i64; clamp instead of failing.
fn as_i64_saturating(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX)))
}

fn helper_error(value: &Value) -> Option<Vec<OutputMsg>> {
    let msg = value.get("msg")?.as_str()?.to_string();
    let helper = value
        .get("helper")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let line = value
        .get("line")
        .and_then(Value::as_u64)
        .and_then(|l| u32::try_from(l).ok());
    Some(vec![OutputMsg::HelperError { msg, helper, line }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");

    fn fixture(name: &str) -> Vec<OutputMsg> {
        let text = std::fs::read_to_string(format!("{FIXTURES}/{name}")).expect("fixture");
        text.lines().flat_map(parse_line).collect()
    }

    fn b(min: Option<i64>, max: Option<i64>, count: u64) -> Bucket {
        Bucket { min, max, count }
    }

    fn map(name: &str, value: MapValue) -> OutputMsg {
        OutputMsg::Map {
            name: name.into(),
            value,
        }
    }

    fn keyed(pairs: &[(&str, Value)]) -> MapValue {
        MapValue::Keyed(pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    #[test]
    fn every_fixture_parses_without_unknowns() {
        let mut names: Vec<_> = std::fs::read_dir(FIXTURES)
            .expect("dir")
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".ndjson"))
            .collect();
        names.sort();
        assert!(names.len() >= 18, "{names:?}");
        for name in names {
            for msg in fixture(&name) {
                assert!(
                    !matches!(msg, OutputMsg::Unknown(_) | OutputMsg::NotJson(_)),
                    "{name}: {msg:?}"
                );
            }
        }
    }

    #[test]
    fn attached_probes() {
        assert_eq!(
            fixture("attached_probes.ndjson"),
            vec![OutputMsg::AttachedProbes(1)]
        );
        // Older versions: no top-level count. Hypothetical newer: count only.
        assert_eq!(
            parse_line(r#"{"type":"attached_probes","data":{"probes":7}}"#),
            vec![OutputMsg::AttachedProbes(7)]
        );
        assert_eq!(
            parse_line(r#"{"type":"attached_probes","count":2}"#),
            vec![OutputMsg::AttachedProbes(2)]
        );
    }

    #[test]
    fn text_kinds() {
        assert_eq!(
            fixture("printf.ndjson"),
            vec![OutputMsg::Text {
                kind: TextKind::Printf,
                text: "test 5".into()
            }]
        );
        assert_eq!(
            fixture("time.ndjson"),
            vec![OutputMsg::Text {
                kind: TextKind::Time,
                text: "12:34:56\n".into()
            }]
        );
        for (ty, kind) in [
            ("cat", TextKind::Cat),
            ("join", TextKind::Join),
            ("syscall", TextKind::Syscall),
        ] {
            let line = format!(r#"{{"type":"{ty}","data":"x y\n"}}"#);
            assert_eq!(
                parse_line(&line),
                vec![OutputMsg::Text {
                    kind,
                    text: "x y\n".into()
                }]
            );
        }
    }

    #[test]
    fn values() {
        assert_eq!(
            fixture("value.ndjson"),
            vec![
                OutputMsg::Value(json!(5)),
                OutputMsg::Value(json!({"m": 2, "n": 3}))
            ]
        );
    }

    #[test]
    fn maps() {
        assert_eq!(
            fixture("map.ndjson"),
            vec![map("@map", keyed(&[("key1", json!(2)), ("key2", json!(3))]))]
        );
        assert_eq!(
            fixture("multiple_maps.ndjson"),
            vec![
                map("@map1", keyed(&[("key1", json!(2))])),
                map("@map2", keyed(&[("key2", json!(3))]))
            ]
        );
        assert_eq!(
            fixture("complex.ndjson"),
            vec![map("@complex", keyed(&[("bpftrace,2", json!(5))]))]
        );
        assert_eq!(
            fixture("scalar_str.ndjson"),
            vec![map("@scalar_str", MapValue::Scalar(json!("a b \n d e")))]
        );
        assert_eq!(
            parse_line(r#"{"type":"map","data":{"@x":5}}"#),
            vec![map("@x", MapValue::Scalar(json!(5)))]
        );
    }

    #[test]
    fn keyed_map_keeps_bpftrace_order() {
        let msgs = parse_line(r#"{"type":"map","data":{"@m":{"zeta":1,"alpha":2,"10":3,"9":4}}}"#);
        let OutputMsg::Map {
            value: MapValue::Keyed(entries),
            ..
        } = &msgs[0]
        else {
            panic!("{msgs:?}")
        };
        let keys: Vec<_> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["zeta", "alpha", "10", "9"]);
    }

    #[test]
    fn hists() {
        let msgs = fixture("hist.ndjson");
        let OutputMsg::Hist {
            name,
            series: HistSeries::Single(buckets),
        } = &msgs[0]
        else {
            panic!("{msgs:?}")
        };
        assert_eq!(name, "@hist");
        assert_eq!(buckets.len(), 10);
        assert_eq!(buckets[0], b(Some(2), Some(3), 1));
        assert_eq!(buckets[9], b(Some(1024), Some(2047), 1));

        assert_eq!(
            fixture("hist_zero.ndjson"),
            vec![OutputMsg::Hist {
                name: "@hist".into(),
                series: HistSeries::Single(vec![])
            }]
        );
    }

    #[test]
    fn lhist_overflow_and_underflow_buckets() {
        let msgs = fixture("lhist.ndjson");
        let OutputMsg::Hist {
            series: HistSeries::Single(buckets),
            ..
        } = &msgs[0]
        else {
            panic!("{msgs:?}")
        };
        assert_eq!(buckets.last(), Some(&b(Some(100), None, 1)));

        let msgs = fixture("hist_multiple.ndjson");
        let OutputMsg::Hist {
            series: HistSeries::Keyed(series),
            ..
        } = &msgs[0]
        else {
            panic!("{msgs:?}")
        };
        let keys: Vec<_> = series.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["bpftrace", "curl"]);
        assert_eq!(series[1].1[0], b(None, Some(-1), 1));
        assert_eq!(series[1].1.len(), 13);
    }

    #[test]
    fn keyed_hist_with_int_keys() {
        let msgs = fixture("hist_keyed_ints.ndjson");
        assert_eq!(
            msgs,
            vec![OutputMsg::Hist {
                name: "@".into(),
                series: HistSeries::Keyed(vec![
                    ("2".into(), vec![b(Some(16), Some(31), 1)]),
                    ("3".into(), vec![b(Some(16), Some(31), 1)]),
                ])
            }]
        );
    }

    #[test]
    fn huge_bucket_bounds_saturate() {
        let msgs = parse_line(
            r#"{"type":"hist","data":{"@h":[{"min":9223372036854775808,"max":18446744073709551615,"count":1}]}}"#,
        );
        assert_eq!(
            msgs,
            vec![OutputMsg::Hist {
                name: "@h".into(),
                series: HistSeries::Single(vec![b(Some(i64::MAX), Some(i64::MAX), 1)])
            }]
        );
    }

    #[test]
    fn stats_and_tseries_are_kept_generic() {
        assert_eq!(
            fixture("stats.ndjson"),
            vec![OutputMsg::Stats {
                name: "@stats".into(),
                value: json!({"count": 2, "average": 6, "total": 12})
            }]
        );
        assert_eq!(
            fixture("stats_keyed.ndjson"),
            vec![OutputMsg::Stats {
                name: "@".into(),
                value: json!({"c": 3, "d": 4})
            }]
        );
        let msgs = fixture("tseries.ndjson");
        let OutputMsg::Tseries { name, value } = &msgs[0] else {
            panic!("{msgs:?}")
        };
        assert_eq!(name, "@a");
        assert_eq!(value.as_array().map(Vec::len), Some(5));
    }

    #[test]
    fn helper_error() {
        assert_eq!(
            fixture("helper_error.ndjson"),
            vec![OutputMsg::HelperError {
                msg: "Bad address".into(),
                helper: "probe_read_user".into(),
                line: Some(1)
            }]
        );
    }

    #[test]
    fn session_mixed_in_order() {
        let kinds: Vec<_> = fixture("session_mixed.ndjson")
            .iter()
            .map(|m| match m {
                OutputMsg::AttachedProbes(_) => "attached",
                OutputMsg::Text { .. } => "text",
                OutputMsg::Map { .. } => "map",
                OutputMsg::Hist { .. } => "hist",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["attached", "text", "map", "map", "hist"]);
    }

    #[test]
    fn unknown_and_malformed_lines_are_data() {
        let cases = [
            r#"{"type":"benchmark_result","data":{"x":1}}"#,
            r#"{"type":"lost_events","data":{"events":3}}"#,
            r#"{"no_type":1}"#,
            r#"{"type":"printf","data":5}"#,
            r#"{"type":"hist","data":{"@h":"nope"}}"#,
            r#"{"type":"hist","data":{"@h":[{"count":1}]}}"#,
            r#"{"type":"map","data":{}}"#,
            r#"{"type":"map"}"#,
            r#"{"type":"helper_error"}"#,
            r#"[1,2]"#,
            r#"42"#,
        ];
        for line in cases {
            let msgs = parse_line(line);
            assert!(
                matches!(msgs.as_slice(), [OutputMsg::Unknown(_)]),
                "{line}: {msgs:?}"
            );
        }
        assert_eq!(
            parse_line("WARNING: could not resolve symbol"),
            vec![OutputMsg::NotJson("WARNING: could not resolve symbol".into())]
        );
        assert_eq!(
            parse_line(r#"{"type":"printf","data":"x"#),
            vec![OutputMsg::NotJson(r#"{"type":"printf","data":"x"#.into())]
        );
        assert_eq!(parse_line("   \r"), vec![]);
    }

    #[test]
    fn one_message_per_map_name() {
        let msgs = parse_line(r#"{"type":"map","data":{"@a":1,"@b":{"k":2}}}"#);
        assert_eq!(
            msgs,
            vec![
                map("@a", MapValue::Scalar(json!(1))),
                map("@b", keyed(&[("k", json!(2))]))
            ]
        );
    }
}
