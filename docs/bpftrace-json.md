# bpftrace JSON output — what bpfdeck consumes

Source of truth: `bpftrace(8)` (`-f json`) and bpftrace's own tests
(`tests/runtime/json-output`, `tests/runtime/outputs/`). Fixtures in
`tests/fixtures/json/` are copies of those, one message per line.

## Transport

- `-f json` emits **NDJSON**: one complete JSON object per line on stdout.
- Always pass `-B line` so each message is flushed as it is produced.
- Errors and most warnings go to **stderr as plain text**, not JSON. Some older
  versions leak non-JSON lines to stdout → treat unparseable stdout lines as raw log text.
- Every message has a string field `type`. Most carry their payload in `data`;
  some carry extra top-level fields (`helper_error`, `attached_probes.count`).

## Message types

| `type` | Shape of `data` | Notes |
|---|---|---|
| `attached_probes` | `{"probes": N}` | Newer versions also add top-level `"count": N`. Emitted once after attach. |
| `printf` | string | Text as formatted, may contain `\n`. Split into log lines. |
| `time` | string | From `time()`. |
| `cat`, `join`, `syscall` | string | From `cat()`, `join()`, `system()`. |
| `value` | any JSON | From `print(<non-map value>)`. |
| `map` | `{"@name": <value>}` | `<value>` is a scalar (`@x = 5`), a string, an array/tuple, or an object of key → value for keyed maps. Tuple keys are joined with `,` (`"bpftrace,2"`). Integer keys arrive as strings. |
| `hist` | `{"@name": [bucket…]}` or `{"@name": {"key": [bucket…], …}}` | Used for **both** `hist()` and `lhist()`. |
| `stats` | `{"@name": {"count","average","total"}}` or keyed object of those | From `stats()`/`avg()`-like aggregations. Some shapes use arbitrary keys → render generically. |
| `tseries` | `{"@name": [{"interval_start": "…", "value": N}, …]}` | `interval_start` is a timestamp string. Can also be keyed. |
| `helper_error` | none — top-level `msg`, `helper`, `retcode`, `filename`, `line`, `col` | Render as error log line. |
| `benchmark_result` | object | Ignore (log muted). |
| anything else | anything | Log as raw JSON, muted. Never fail. |

### Histogram bucket

```json
{"min": 16, "max": 31, "count": 210}
```

- `min` **or** `max` may be missing: underflow bucket `{"max": -1, "count": 1}`,
  overflow bucket `{"min": 100, "count": 1}`.
- Labels: log2 `hist` → `[16, 32)` style (max+1), linear `lhist` → `[10, 20)`.
  Underflow → `(..., 0)`, overflow → `[100, ...)`. Detection of log2 vs linear is not
  needed for labels if `[min, max+1)` is used everywhere.
- Buckets with zero count can appear in the middle; keep them (they are visual gaps).

## Rust model (`src/bpftrace/json.rs`)

```rust
pub enum OutputMsg {
    AttachedProbes(u64),
    Text { kind: TextKind, text: String },        // printf/time/cat/join/syscall
    Value(serde_json::Value),
    Map { name: String, value: MapValue },
    Hist { name: String, series: HistSeries },    // Single(Vec<Bucket>) | Keyed(Vec<(String, Vec<Bucket>)>)
    Stats { name: String, value: serde_json::Value },
    Tseries { name: String, value: serde_json::Value },
    HelperError { msg: String, helper: String, line: Option<u32> },
    Unknown(serde_json::Value),                   // keep raw
    NotJson(String),                              // stdout line that failed to parse
}

pub enum MapValue {
    Scalar(serde_json::Value),                    // number/string/bool/array
    Keyed(Vec<(String, serde_json::Value)>),      // preserve bpftrace order
}
```

Parse with `serde_json::Value` first, then dispatch on `type` by hand. A strict
`#[serde(tag = "type")]` enum breaks on every new bpftrace field — don't.

`data` objects have exactly one `@name` key in practice, but loop over all keys.
Enable serde_json's `preserve_order` feature so keyed maps keep bpftrace's order.

## Lifecycle of a typical run

```
{"type":"attached_probes",…}          → header: 3 probes
{"type":"printf","data":"Tracing…\n"}  → log
{"type":"map","data":{"@x":{…}}}       → table (interval print)
{"type":"map","data":{"@x":{…}}}       → table replaced, deltas shown
<SIGINT>
{"type":"hist","data":{"@usecs":[…]}}  → END / exit-time map dump
<exit 0>
```

On SIGINT bpftrace prints all non-empty maps before exiting. That final dump is often
the whole point of the script (biolatency, runqlat…) — it must be captured.

## To verify on real machines (M2 exit criterion)

- [ ] RHEL 9 packaged bpftrace: capture output of every fixture script → `real_rhel9_*.ndjson`.
- [~] Debian 12/13 packaged bpftrace: Debian 13 (0.23.2) done on a Docker VM kernel, see
      docs/real-kernel-testing.md → `real_debian13_orbstack_*.ndjson`. Debian 12 and a real
      host still to do.
- [ ] RHEL 8 packaged bpftrace: does it support `--dry-run`? Does `-f json` match the above?
- [x] Does stdout ever contain non-JSON lines with `-q`? 0.23.2: only blank lines (2).
- [x] Exit-time dump order and whether `attached_probes` has `count`. 0.23.2: blank lines,
      then the maps; no `count` (and no `attached_probes` at all with `-q`).
