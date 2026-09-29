//! Source tab: line numbers + light, hand-written syntax highlighting (spec §5.1).

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::theme::Theme;
use crate::discovery::lexer::{self, Region};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Comment,
    String,
    Probe,
    Builtin,
    Map,
    Var,
}

/// Builtin variables, functions and keywords worth a color. Not exhaustive on purpose.
const BUILTINS: &[&str] = &[
    // variables
    "pid",
    "tid",
    "uid",
    "gid",
    "nsecs",
    "elapsed",
    "cpu",
    "comm",
    "kstack",
    "ustack",
    "args",
    "retval",
    "func",
    "probe",
    "curtask",
    "rand",
    "cgroup",
    "username",
    "jiffies",
    "arg0",
    "arg1",
    "arg2",
    "arg3",
    "arg4",
    "arg5",
    // functions
    "printf",
    "print",
    "time",
    "join",
    "str",
    "buf",
    "ksym",
    "usym",
    "kaddr",
    "uaddr",
    "ntop",
    "cat",
    "exit",
    "system",
    "signal",
    "override",
    "clear",
    "zero",
    "delete",
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "stats",
    "hist",
    "lhist",
    "tseries",
    "len",
    "strftime",
    "macaddr",
    "path",
    "strncmp",
    "strcontains",
    "has_key",
    "sizeof",
    "offsetof",
    "getopt",
    "errorf",
    "warnf",
    // keywords
    "if",
    "else",
    "while",
    "for",
    "unroll",
    "return",
    "let",
    "macro",
    "fn",
    "import",
    "config",
];

/// Split `src` into lines of `(kind, text)` segments.
pub fn classify(src: &str) -> Vec<Vec<(Kind, &str)>> {
    let bytes = src.as_bytes();
    let mut kinds: Vec<Kind> = lexer::regions(src)
        .into_iter()
        .map(|r| match r {
            Region::Code => Kind::Plain,
            Region::Comment => Kind::Comment,
            Region::Quote | Region::StringBody => Kind::String,
        })
        .collect();

    for block in lexer::probe_blocks(&lexer::mask(src)) {
        for k in block.list_span {
            if kinds[k] == Kind::Plain && !bytes[k].is_ascii_whitespace() {
                kinds[k] = Kind::Probe;
            }
        }
    }

    // Words in plain code: @maps, $vars, builtins.
    let mut i = 0;
    while i < bytes.len() {
        if kinds[i] != Kind::Plain {
            i += 1;
            continue;
        }
        let c = bytes[i];
        let word_end = |from: usize| {
            let mut j = from;
            while j < bytes.len()
                && kinds[j] == Kind::Plain
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_')
            {
                j += 1;
            }
            j
        };
        if c == b'@' || c == b'$' {
            let mut end = word_end(i + 1);
            if c == b'$' && end == i + 1 && bytes.get(i + 1) == Some(&b'#') {
                end += 1;
            }
            let kind = if c == b'@' { Kind::Map } else { Kind::Var };
            kinds[i..end].fill(kind);
            i = end.max(i + 1);
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let end = word_end(i);
            let preceded_by_dot = i > 0 && bytes[i - 1] == b'.';
            if !preceded_by_dot && BUILTINS.contains(&&src[i..end]) {
                kinds[i..end].fill(Kind::Builtin);
            }
            i = end;
        } else {
            i += 1;
        }
    }

    let mut lines = Vec::new();
    let mut start = 0;
    for line in src.split('\n') {
        let end = start + line.len();
        let mut segments = Vec::new();
        let mut seg_start = start;
        for k in start + 1..=end {
            if k == end || kinds[k] != kinds[seg_start] {
                if k > seg_start {
                    segments.push((kinds[seg_start], &src[seg_start..k]));
                }
                seg_start = k;
            }
        }
        lines.push(segments);
        start = end + 1;
    }
    // A trailing newline does not start a line worth numbering.
    if src.ends_with('\n') {
        lines.pop();
    }
    lines
}

pub(super) fn style(kind: Kind) -> Style {
    match kind {
        Kind::Plain => Style::new(),
        Kind::Comment => Theme::code_comment(),
        Kind::String => Theme::code_string(),
        Kind::Probe => Theme::code_probe(),
        Kind::Builtin => Theme::code_builtin(),
        Kind::Map => Theme::code_map(),
        Kind::Var => Theme::code_var(),
    }
}

/// Highlighted lines with a right-aligned line-number gutter.
pub fn lines(src: &str) -> Vec<Line<'_>> {
    let classified = classify(src);
    let width = classified.len().max(1).to_string().len();
    classified
        .into_iter()
        .enumerate()
        .map(|(n, segments)| {
            let mut spans = vec![Span::styled(format!("{:>width$} ", n + 1), Theme::line_number())];
            // Tabs would be drawn as a single cell; expand them so columns line up.
            spans.extend(
                segments
                    .into_iter()
                    .map(|(kind, text)| Span::styled(text.replace('\t', "    "), style(kind))),
            );
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn kinds_of<'a>(line: &[(Kind, &'a str)]) -> Vec<(Kind, &'a str)> {
        line.iter()
            .filter(|(_, t)| !t.trim().is_empty())
            .copied()
            .collect()
    }

    #[test]
    fn classifies_a_script() {
        let src = "// hdr {\nkprobe:vfs_read,\n  kretprobe:vfs_read /pid == $1/\n{\n  @s[tid] = str(\"x{\");\n  args.comm;\n}\n";
        let lines = classify(src);
        assert_eq!(lines.len(), 7);
        assert_eq!(lines[0], vec![(Kind::Comment, "// hdr {")]);
        assert_eq!(kinds_of(&lines[1]), vec![(Kind::Probe, "kprobe:vfs_read,")]);
        assert_eq!(
            kinds_of(&lines[2]),
            vec![
                (Kind::Probe, "kretprobe:vfs_read"),
                (Kind::Plain, " /"),
                (Kind::Builtin, "pid"),
                (Kind::Plain, " == "),
                (Kind::Var, "$1"),
                (Kind::Plain, "/"),
            ]
        );
        assert_eq!(
            kinds_of(&lines[4]),
            vec![
                (Kind::Map, "@s"),
                (Kind::Plain, "["),
                (Kind::Builtin, "tid"),
                (Kind::Plain, "] = "),
                (Kind::Builtin, "str"),
                (Kind::Plain, "("),
                (Kind::String, "\"x{\""),
                (Kind::Plain, ");"),
            ]
        );
        // `args.comm`: `args` is a builtin, `.comm` is a field, not the `comm` builtin.
        assert_eq!(
            kinds_of(&lines[5]),
            vec![(Kind::Builtin, "args"), (Kind::Plain, ".comm;")]
        );
    }

    #[test]
    fn argc_and_multibyte_text() {
        let lines = classify("BEGIN { if ($# > 0) { printf(\"ünïcode ✓\"); } } // çok güzel");
        let flat: Vec<_> = lines[0].iter().map(|(k, t)| (*k, *t)).collect();
        assert!(flat.contains(&(Kind::Var, "$#")));
        assert!(flat.contains(&(Kind::String, "\"ünïcode ✓\"")));
        assert!(flat.contains(&(Kind::Comment, "// çok güzel")));
    }

    #[test]
    fn never_panics_on_fixture_prefixes() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/scripts/vfs_latency_demo.bt"
        ))
        .expect("fixture");
        for end in (0..=src.len()).filter(|&i| src.is_char_boundary(i)) {
            let _ = lines(&src[..end]);
        }
        assert_eq!(lines(&src).len(), src.lines().count());
    }
}
