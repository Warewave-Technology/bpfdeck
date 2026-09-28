//! One-line text renderings of bpftrace output messages, for logs and headless output.

use crate::bpftrace::json::{Bucket, HistSeries, MapValue, OutputMsg};

pub fn describe(msg: &OutputMsg) -> String {
    match msg {
        OutputMsg::AttachedProbes(n) => format!("attached {n} probes"),
        OutputMsg::Text { kind, text } => format!("{kind:?}: {}", text.trim_end()),
        OutputMsg::Value(v) => format!("value: {v}"),
        OutputMsg::Map { name, value } => match value {
            MapValue::Scalar(v) => format!("map {name} = {v}"),
            MapValue::Keyed(entries) => {
                let shown: Vec<_> = entries.iter().take(10).map(|(k, v)| format!("{k}={v}")).collect();
                let more = entries.len().saturating_sub(shown.len());
                let more = if more > 0 {
                    format!(" …+{more}")
                } else {
                    String::new()
                };
                format!("map {name}: {}{more}", shown.join(", "))
            }
        },
        OutputMsg::Hist { name, series } => match series {
            HistSeries::Single(buckets) => format!("hist {name}: {}", buckets_str(buckets)),
            HistSeries::Keyed(keyed) => {
                let parts: Vec<_> = keyed
                    .iter()
                    .map(|(k, b)| format!("[{k}] {}", buckets_str(b)))
                    .collect();
                format!("hist {name}: {}", parts.join(" | "))
            }
        },
        OutputMsg::Stats { name, value } => format!("stats {name}: {value}"),
        OutputMsg::Tseries { name, value } => format!("tseries {name}: {value}"),
        OutputMsg::HelperError { msg, helper, line } => {
            let at = line.map(|l| format!(" (line {l})")).unwrap_or_default();
            format!("helper_error {helper}: {msg}{at}")
        }
        OutputMsg::Unknown(v) => format!("unknown: {v}"),
        OutputMsg::NotJson(s) => format!("raw: {s}"),
    }
}

/// `[16, 32):210` per non-empty bucket, bpftrace-style bounds (docs/bpftrace-json.md).
fn buckets_str(buckets: &[Bucket]) -> String {
    let parts: Vec<_> = buckets
        .iter()
        .filter(|b| b.count > 0)
        .map(|b| {
            let label = match (b.min, b.max) {
                (Some(min), Some(max)) => format!("[{min}, {})", max.saturating_add(1)),
                (None, Some(max)) => format!("(..., {})", max.saturating_add(1)),
                (Some(min), None) => format!("[{min}, ...)"),
                (None, None) => "?".to_string(),
            };
            format!("{label}:{}", b.count)
        })
        .collect();
    if parts.is_empty() {
        "(empty)".to_string()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpftrace::json::parse_line;

    #[test]
    fn describes_fixture_messages() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/json");
        let text = |f: &str| std::fs::read_to_string(format!("{dir}/{f}")).expect("fixture");
        let lines: Vec<String> = [
            "session_mixed.ndjson",
            "lhist.ndjson",
            "hist_multiple.ndjson",
            "helper_error.ndjson",
        ]
        .iter()
        .flat_map(|f| text(f).lines().flat_map(parse_line).collect::<Vec<_>>())
        .map(|m| describe(&m))
        .collect();
        insta::assert_snapshot!(lines.join("\n"));
    }
}
