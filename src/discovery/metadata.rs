//! Metadata extraction from script text (spec §6.3). Pure and panic-free on any input.

use std::sync::LazyLock;

use regex::Regex;

use super::lexer::{self, Masked};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    /// One-line description from the header comment.
    pub description: Option<String>,
    /// `USAGE:` / `Example of usage:` lines from the header, marker kept.
    pub usage: Vec<String>,
    /// Probe specs in source order, as written (wildcards included), deduplicated.
    pub probes: Vec<Probe>,
    pub params: Params,
    /// Calls that make bpftrace demand `--unsafe` (e.g. `system`). A hint only:
    /// the dry-run stderr is the source of truth.
    pub unsafe_calls: Vec<String>,
}

impl Metadata {
    pub fn needs_unsafe(&self) -> bool {
        !self.unsafe_calls.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub spec: String,
    /// 1-based line of the block this probe belongs to.
    pub line: usize,
    /// `BEGIN`, `interval:…` and friends: no kernel/user symbol to look up.
    pub always_available: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Params {
    /// Positional parameter numbers used (`$1` → 1), sorted, deduplicated.
    pub positional: Vec<u32>,
    /// The script reads `$#` (argument count).
    pub uses_argc: bool,
    /// `getopt("name"[, default])` calls, first occurrence wins.
    pub named: Vec<NamedParam>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedParam {
    pub name: String,
    /// Default as written in the source (string literals keep their quotes).
    pub default: Option<String>,
    /// No default, or a `true`/`false` default → a flag (`--name`).
    pub is_bool: bool,
    /// Optional third argument (bpftrace ≥ 0.24): `getopt("depth", 35, "Max depth")`.
    pub description: Option<String>,
}

pub fn extract(src: &str) -> Metadata {
    let masked = lexer::mask(src);
    let header = header_lines(src);
    Metadata {
        description: description(&header),
        usage: usage(&header),
        probes: probes(&masked),
        params: params(&masked),
        unsafe_calls: unsafe_calls(&masked),
    }
}

/// Text of the leading comment block: after the shebang, before the first code.
/// `//` markers and block-comment decoration (`/*`, ` * `, `*/`) are stripped.
fn header_lines(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for (n, raw) in src.lines().enumerate() {
        let line = raw.trim();
        if n == 0 && line.starts_with("#!") {
            continue;
        }
        if in_block {
            let (text, closed) = match line.find("*/") {
                Some(end) => (&line[..end], true),
                None => (line, false),
            };
            out.push(strip_star(text));
            in_block = !closed;
        } else if let Some(rest) = line.strip_prefix("//") {
            out.push(strip_one_space(rest).trim_end().to_string());
        } else if let Some(rest) = line.strip_prefix("/*") {
            match rest.find("*/") {
                Some(end) => out.push(strip_star(&rest[..end])),
                None => {
                    out.push(strip_star(rest));
                    in_block = true;
                }
            }
        } else if line.is_empty() {
            // Blank lines between the shebang and the first comment are fine; after the
            // header started they end it.
            if !out.is_empty() {
                break;
            }
        } else {
            break;
        }
    }
    out
}

fn strip_star(text: &str) -> String {
    let t = text.trim_start();
    let t = t.strip_prefix('*').map(strip_one_space).unwrap_or(t);
    t.trim_end().to_string()
}

fn strip_one_space(s: &str) -> &str {
    s.strip_prefix(' ').unwrap_or(s)
}

fn is_usage_start(line: &str) -> bool {
    let t = line.trim_start();
    ["USAGE:", "Usage:", "usage:", "Example of usage:"]
        .iter()
        .any(|m| t.starts_with(m))
}

/// bpftrace/tools convention: `name.bt<TAB>Description.`. The first word is treated as a
/// name when it ends in `.bt` or is followed by a tab / 2+ spaces.
fn description(header: &[String]) -> Option<String> {
    static NAMED: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?:[\w.\-]+\.bt\s+|[\w.\-]+(?:\t|\s{2,})\s*)(\S.*)$").expect("static regex")
    });
    let line = header
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !is_usage_start(l))?;
    let text = NAMED
        .captures(line)
        .and_then(|c| c.get(1))
        .map_or(line, |m| m.as_str());
    Some(text.trim().to_string())
}

fn usage(header: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < header.len() {
        let line = &header[i];
        if !is_usage_start(line) {
            i += 1;
            continue;
        }
        out.push(line.trim_start().to_string());
        i += 1;
        // "Example of usage:" is usually followed by a blank line before the example.
        if line.trim() == "Example of usage:" && header.get(i).is_some_and(|l| l.trim().is_empty()) {
            i += 1;
        }
        while let Some(next) = header.get(i) {
            if next.trim().is_empty() || is_usage_start(next) {
                break;
            }
            out.push(next.trim_end().to_string());
            i += 1;
        }
    }
    out
}

fn probes(masked: &Masked) -> Vec<Probe> {
    let mut out: Vec<Probe> = Vec::new();
    for block in lexer::probe_blocks(masked) {
        for spec in block.probes {
            if out.iter().any(|p| p.spec == spec) {
                continue;
            }
            out.push(Probe {
                always_available: is_always_available(&spec),
                spec,
                line: block.line,
            });
        }
    }
    out
}

/// Probe types that exist on every kernel bpftrace runs on (aliases included).
fn is_always_available(spec: &str) -> bool {
    const EXACT: &[&str] = &["BEGIN", "END", "begin", "end"];
    const TYPES: &[&str] = &[
        "interval", "i", "profile", "p", "software", "s", "hardware", "h", "self",
    ];
    EXACT.contains(&spec) || spec.split_once(':').is_some_and(|(ty, _)| TYPES.contains(&ty))
}

fn params(masked: &Masked) -> Params {
    static POSITIONAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$([0-9]+)").expect("static regex"));
    // getopt("name"[, default[, "description"]]); a string default may contain commas.
    static GETOPT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"getopt\(\s*"([^"]+)"\s*(?:,\s*("[^"]*"|[^,)]+?)\s*(?:,\s*"([^"]*)"\s*)?)?\)"#)
            .expect("static regex")
    });

    // Positional/argc on the string-masked view (`printf("$1")` is not a parameter);
    // getopt on the comment-free code, because the name is a string literal.
    let structure = masked.structure_str();
    let mut positional: Vec<u32> = POSITIONAL
        .captures_iter(&structure)
        .filter_map(|c| c.get(1)?.as_str().parse().ok())
        .filter(|&n| n > 0)
        .collect();
    positional.sort_unstable();
    positional.dedup();

    let mut named: Vec<NamedParam> = Vec::new();
    for c in GETOPT.captures_iter(&masked.code) {
        let Some(name) = c.get(1).map(|m| m.as_str().to_string()) else {
            continue;
        };
        if named.iter().any(|p| p.name == name) {
            continue;
        }
        let default = c.get(2).map(|m| m.as_str().trim().to_string());
        let is_bool = default.as_deref().is_none_or(|d| d == "true" || d == "false");
        named.push(NamedParam {
            name,
            default,
            is_bool,
            description: c.get(3).map(|m| m.as_str().to_string()),
        });
    }

    Params {
        positional,
        uses_argc: structure.contains("$#"),
        named,
    }
}

fn unsafe_calls(masked: &Masked) -> Vec<String> {
    // Group 1 catches definitions (`macro override(expr)` in the stdlib), which are not calls.
    static UNSAFE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|[^\w@$.])(?:(fn|macro)\s+)?(system|signal|override|write_user)\s*\(")
            .expect("static regex")
    });
    let structure = masked.structure_str();
    let mut out: Vec<String> = Vec::new();
    for c in UNSAFE.captures_iter(&structure) {
        if c.get(1).is_none()
            && let Some(name) = c.get(2).map(|m| m.as_str().to_string())
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scripts");

    fn fixture(rel: &str) -> Metadata {
        let src = std::fs::read_to_string(format!("{FIXTURES}/{rel}")).expect("fixture");
        extract(&src)
    }

    fn specs(m: &Metadata) -> Vec<&str> {
        m.probes.iter().map(|p| p.spec.as_str()).collect()
    }

    fn named(name: &str, default: Option<&str>, is_bool: bool) -> NamedParam {
        NamedParam {
            name: name.into(),
            default: default.map(Into::into),
            is_bool,
            description: None,
        }
    }

    /// (fixture, description, probes, positional, named names, unsafe calls)
    type Row = (
        &'static str,
        Option<&'static str>,
        &'static [&'static str],
        &'static [u32],
        &'static [&'static str],
        &'static [&'static str],
    );

    #[test]
    fn every_fixture() {
        let table: &[Row] = &[
            (
                "missing_probe_demo.bt",
                Some("Attaches to a kernel function that does not exist."),
                &["kprobe:this_function_does_not_exist_bpfdeck"],
                &[],
                &[],
                &[],
            ),
            (
                "no_header.bt",
                None,
                &["tracepoint:sched:sched_process_exec"],
                &[],
                &[],
                &[],
            ),
            (
                "params_demo.bt",
                Some("Positional and named parameters fixture."),
                &["tracepoint:syscalls:sys_enter_openat", "interval:s:1"],
                &[1],
                &["verbose"],
                &[],
            ),
            (
                "shebang_no_ext",
                Some("Discovered via shebang, not extension."),
                &["BEGIN"],
                &[],
                &[],
                &[],
            ),
            (
                "syscount_demo.bt",
                Some("Count syscalls by process name, printed every second."),
                &["tracepoint:raw_syscalls:sys_enter", "interval:s:1"],
                &[],
                &[],
                &[],
            ),
            (
                "unsafe_demo.bt",
                Some("Uses system(); requires --unsafe. bpfdeck must flag this"),
                &["BEGIN"],
                &[],
                &[],
                &["system"],
            ),
            (
                "vfs_latency_demo.bt",
                Some("vfs_read latency as a log2 histogram (usecs)."),
                &["BEGIN", "kprobe:vfs_read", "kretprobe:vfs_read", "END"],
                &[],
                &[],
                &[],
            ),
            (
                "net/tcpconnect_demo.bt",
                Some("Print TCP connect() calls. Fixture for printf/log rendering"),
                &["kprobe:tcp_connect"],
                &[],
                &[],
                &[],
            ),
        ];
        for (file, desc, probes, positional, named, unsafe_calls) in table {
            let m = fixture(file);
            assert_eq!(m.description.as_deref(), *desc, "{file}: description");
            assert_eq!(specs(&m), *probes, "{file}: probes");
            assert_eq!(m.params.positional, *positional, "{file}: positional");
            let names: Vec<_> = m.params.named.iter().map(|p| p.name.as_str()).collect();
            assert_eq!(names, *named, "{file}: named");
            assert_eq!(m.unsafe_calls, *unsafe_calls, "{file}: unsafe");
        }
    }

    #[test]
    fn params_demo_usage_block() {
        let m = fixture("params_demo.bt");
        assert_eq!(
            m.usage,
            vec![
                "USAGE: params_demo.bt <pid> -- --interval=5 --verbose",
                "  $1          target pid (positional, required)",
                "  --interval  print interval in seconds (named, default 1)",
                "  --verbose   boolean flag",
            ]
        );
        assert_eq!(m.params.named, vec![named("verbose", None, true)]);
        assert!(!m.params.uses_argc);
    }

    #[test]
    fn probe_lines_and_availability() {
        let m = fixture("vfs_latency_demo.bt");
        let got: Vec<_> = m
            .probes
            .iter()
            .map(|p| (p.spec.as_str(), p.line, p.always_available))
            .collect();
        assert_eq!(
            got,
            vec![
                ("BEGIN", 5, true),
                ("kprobe:vfs_read", 10, false),
                ("kretprobe:vfs_read", 15, false),
                ("END", 22, true)
            ]
        );
    }

    #[test]
    fn always_available() {
        for p in [
            "BEGIN",
            "end",
            "interval:s:1",
            "i:ms:100",
            "profile:hz:99",
            "software:cpu-clock:1",
            "self:signal:SIGUSR1",
        ] {
            assert!(is_always_available(p), "{p}");
        }
        for p in [
            "kprobe:f",
            "tracepoint:a:b",
            "BEGINX",
            "uprobe:/bin/sh:main",
            "interval",
        ] {
            assert!(!is_always_available(p), "{p}");
        }
    }

    #[test]
    fn multi_line_probe_list_and_duplicates() {
        let m = extract("kprobe:a,\n  kprobe:b\n/pid == 1/\n{ }\nkprobe:a { }\n");
        assert_eq!(specs(&m), vec!["kprobe:a", "kprobe:b"]);
    }

    #[test]
    fn comments_with_braces() {
        let src =
            "// header { with brace\n/* { kprobe:fake { */\nkprobe:real\n{ // }\n  printf(\"{}\"); \n}\n";
        assert_eq!(specs(&extract(src)), vec!["kprobe:real"]);
    }

    #[test]
    fn named_params_with_defaults() {
        let src = r#"BEGIN { $a = getopt("interval", 5); $b = getopt( "name" , "x"); $c = getopt("on", false);
                    $d = getopt("interval", 9); $e = getopt("flag"); }"#;
        let m = extract(src);
        assert_eq!(
            m.params.named,
            vec![
                named("interval", Some("5"), false),
                named("name", Some("\"x\""), false),
                named("on", Some("false"), true),
                named("flag", None, true),
            ]
        );
    }

    #[test]
    fn named_params_with_descriptions() {
        // Real shapes from bpftrace/tools (opensnoop.bt) and its runtime tests.
        let src = r#"BEGIN { getopt("depth", 35, "Maximum depth of full path");
                    getopt("errname", false, "Show error message instead of errno");
                    getopt("sep", "a,b" , "Separator, default a,b"); getopt("v", $a); getopt("neg", -1) }"#;
        let described = |name: &str, default: &str, is_bool, desc: &str| NamedParam {
            description: Some(desc.into()),
            ..named(name, Some(default), is_bool)
        };
        assert_eq!(
            extract(src).params.named,
            vec![
                described("depth", "35", false, "Maximum depth of full path"),
                described("errname", "false", true, "Show error message instead of errno"),
                described("sep", "\"a,b\"", false, "Separator, default a,b"),
                named("v", Some("$a"), false),
                named("neg", Some("-1"), false),
            ]
        );
    }

    #[test]
    fn positional_params_and_argc() {
        let src = "BEGIN { if ($# < 2) { exit(); } printf(\"$9 %d\", $2 + $1 + $10 + $1); // $7\n }";
        let m = extract(src);
        assert_eq!(m.params.positional, vec![1, 2, 10]);
        assert!(m.params.uses_argc);
    }

    #[test]
    fn unsafe_hints_ignore_strings_comments_and_maps() {
        let src = "// system(\"x\")\nBEGIN { printf(\"system(\"); @system = 1; $signal = 2; \
                   signal(\"KILL\"); override (0); system(\"a\"); }";
        assert_eq!(extract(src).unsafe_calls, vec!["signal", "override", "system"]);
    }

    #[test]
    fn unsafe_definitions_are_not_calls() {
        // bpftrace's own stdlib defines these as macros.
        let src = "macro override(expr) { __override(expr); }
fn  signal(x: int) { }
BEGIN { }";
        assert_eq!(extract(src).unsafe_calls, Vec::<String>::new());
    }

    #[test]
    fn block_comment_header() {
        let src = "#!/usr/bin/env bpftrace\n/*\n * biolatency.bt\tBlock I/O latency as a histogram.\n \
                   *\t\t\tFor Linux, uses bpftrace, eBPF.\n *\n * USAGE: biolatency.bt [-h]\n *   -h  help\n \
                   *\n * Copyright 2018 Netflix\n */\nBEGIN { }";
        let m = extract(src);
        assert_eq!(
            m.description.as_deref(),
            Some("Block I/O latency as a histogram.")
        );
        assert_eq!(m.usage, vec!["USAGE: biolatency.bt [-h]", "  -h  help"]);
    }

    #[test]
    fn description_fallbacks() {
        assert_eq!(
            extract("// Count things by pid\nBEGIN {}").description.as_deref(),
            Some("Count things by pid")
        );
        assert_eq!(
            extract("// tool  Two-space separated.\nBEGIN {}")
                .description
                .as_deref(),
            Some("Two-space separated.")
        );
        assert_eq!(
            extract("//\n// USAGE: x.bt\n// real description\n")
                .description
                .as_deref(),
            Some("real description")
        );
        assert_eq!(extract("BEGIN {}\n// late comment\n").description, None);
        assert_eq!(extract("").description, None);
    }

    #[test]
    fn example_of_usage_block() {
        let src = "// tool.bt\tDoes things.\n//\n// Example of usage:\n//\n// # ./tool.bt\n// Attaching 1 probe...\n//\n// Copyright\nBEGIN {}";
        assert_eq!(
            extract(src).usage,
            vec!["Example of usage:", "# ./tool.bt", "Attaching 1 probe..."]
        );
    }

    /// xorshift64*: deterministic pseudo-random bytes without adding a crate.
    fn rng(seed: u64) -> impl FnMut() -> u64 {
        let mut x = seed;
        move || {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
    }

    #[test]
    fn never_panics_on_random_input() {
        // Biased towards the characters the lexer cares about, to reach odd states.
        const ALPHABET: &[u8] = b"{}/*\"\\\n\t #!,;()=$@:abcgetopt()system";
        let mut next = rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..3000 {
            let len = (next() % 400) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    let r = next();
                    if round % 2 == 0 {
                        ALPHABET[(r % ALPHABET.len() as u64) as usize]
                    } else {
                        r as u8
                    }
                })
                .collect();
            let _ = extract(&String::from_utf8_lossy(&bytes));
        }
    }

    #[test]
    fn never_panics_on_truncated_fixtures() {
        let mut dirs = vec![std::path::PathBuf::from(FIXTURES)];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).expect("fixture dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let src = std::fs::read_to_string(&path).expect("fixture");
                for end in (0..=src.len()).filter(|&i| src.is_char_boundary(i)) {
                    let _ = extract(&src[..end]);
                    let _ = extract(&src[end..]);
                }
            }
        }
    }
}
