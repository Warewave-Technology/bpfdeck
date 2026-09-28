//! Lightweight bpftrace lexer: comment/string masking and top-level probe spec extraction.
//!
//! Not a parser. It only needs to be right about three things: where comments are, where
//! string literals are, and which `{` blocks at brace depth 0 are probe bodies. Everything
//! works on byte offsets so the masked views line up 1:1 with the source.

/// Two views of a script, both exactly as long as the source (in bytes).
#[derive(Debug, Clone)]
pub struct Masked {
    /// Comments replaced by spaces (newlines kept), string literals untouched.
    pub code: String,
    /// Like `code`, but string literal contents (between the quotes) replaced by `x`.
    /// Use this for structural scans so `"{"` or `"system("` inside strings never match.
    pub structure: Vec<u8>,
}

impl Masked {
    /// `structure` as text. Only ASCII bytes are ever written into masked regions, and
    /// unmasked regions are copied from valid UTF-8, so this is lossless in practice.
    pub fn structure_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.structure)
    }
}

/// What each source byte belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Code,
    Comment,
    /// A `"` that opens or closes a string literal.
    Quote,
    /// String literal content, escapes included.
    StringBody,
}

/// Classify every byte of `src`. Strings end at a newline if unterminated; block comments
/// at EOF. Multi-byte characters never straddle two regions.
pub fn regions(src: &str) -> Vec<Region> {
    let bytes = src.as_bytes();
    let mut out = vec![Region::Code; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    out[i] = Region::Comment;
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let end = find_block_comment_end(bytes, i + 2);
                out[i..end].fill(Region::Comment);
                i = end;
            }
            b'"' => {
                out[i] = Region::Quote;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' && bytes[i] != b'\n' {
                    let escaped = bytes[i] == b'\\';
                    out[i] = Region::StringBody;
                    i += 1;
                    if escaped && i < bytes.len() && bytes[i] != b'\n' {
                        out[i] = Region::StringBody;
                        i += 1;
                    }
                }
                if bytes.get(i) == Some(&b'"') {
                    out[i] = Region::Quote;
                }
                // Past the closing quote (or the newline / EOF of an unterminated string).
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

pub fn mask(src: &str) -> Masked {
    let bytes = src.as_bytes();
    let mut code = bytes.to_vec();
    let mut structure = bytes.to_vec();
    for (i, region) in regions(src).into_iter().enumerate() {
        match region {
            Region::Comment if bytes[i] != b'\n' => {
                code[i] = b' ';
                structure[i] = b' ';
            }
            Region::StringBody => structure[i] = b'x',
            _ => {}
        }
    }
    Masked {
        // Whole comments become spaces, so no multi-byte sequence is ever cut in half.
        code: String::from_utf8(code).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
        structure,
    }
}

/// Index just past the closing `*/`, or the end of input for an unterminated comment.
fn find_block_comment_end(bytes: &[u8], from: usize) -> usize {
    let mut j = from;
    while j + 1 < bytes.len() {
        if bytes[j] == b'*' && bytes[j + 1] == b'/' {
            return j + 2;
        }
        j += 1;
    }
    bytes.len()
}

/// One top-level probe block header: `probe[, probe…] [/predicate/] {`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeBlock {
    /// Probe specs as written (comments removed, whitespace trimmed).
    pub probes: Vec<String>,
    /// Predicate text without the slashes, if any.
    pub predicate: Option<String>,
    /// 1-based line of the first probe.
    pub line: usize,
    /// Byte range of the probe list in the source (comments inside it included).
    pub list_span: std::ops::Range<usize>,
}

/// Keywords that open a top-level `{ … }` block which is not a probe.
const NON_PROBE_ITEMS: &[&str] = &[
    "struct", "union", "enum", "typedef", "config", "fn", "macro", "import",
];

/// Find every top-level probe block. Never panics; garbage in → fewer/odd blocks out.
pub fn probe_blocks(masked: &Masked) -> Vec<ProbeBlock> {
    let s = &masked.structure;
    let mut blocks = Vec::new();
    let mut depth: usize = 0;
    // Start of the current top-level item header (after the last `}` or `;`).
    let mut item_start = 0;
    let mut at_line_start = true;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if depth == 0 && at_line_start && c == b'#' {
            // Preprocessor line (#include, #define) or shebang: skip it entirely.
            while i < s.len() && s[i] != b'\n' {
                i += 1;
            }
            item_start = i;
            continue;
        }
        match c {
            b'\n' => at_line_start = true,
            b' ' | b'\t' | b'\r' => {}
            _ => at_line_start = false,
        }
        match c {
            b'{' => {
                if depth == 0
                    && let Some(block) = parse_header(masked, item_start, i)
                {
                    blocks.push(block);
                }
                depth += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    item_start = i + 1;
                }
            }
            b';' if depth == 0 => item_start = i + 1,
            _ => {}
        }
        i += 1;
    }
    blocks
}

fn parse_header(masked: &Masked, start: usize, end: usize) -> Option<ProbeBlock> {
    let header = masked.code.get(start..end)?;
    let structure = masked.structure.get(start..end)?;

    // The predicate starts at the first `/` that follows whitespace: probe paths such as
    // `uprobe:/bin/bash:readline` contain slashes, but never after a blank.
    let pred_start =
        (1..structure.len()).find(|&k| structure[k] == b'/' && structure[k - 1].is_ascii_whitespace());
    let (list, predicate) = match pred_start {
        Some(k) => {
            let pred = header.get(k..)?.trim();
            let pred = pred.strip_prefix('/').unwrap_or(pred);
            let pred = pred.strip_suffix('/').unwrap_or(pred);
            (header.get(..k)?, Some(pred.trim().to_string()))
        }
        None => (header, None),
    };

    let list_structure = &structure[..list.len()];
    let first_word: String = list
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if list.trim().is_empty() || NON_PROBE_ITEMS.contains(&first_word.as_str()) {
        return None;
    }
    // `config = { … }`, `let @x = …` and similar assignments are not probes.
    if list_structure.contains(&b'=') {
        return None;
    }

    let probes = split_probe_list(list, list_structure);
    if probes.is_empty() {
        return None;
    }
    let leading_ws = list.len() - list.trim_start().len();
    let line = masked.code.as_bytes()[..start + leading_ws]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1;
    Some(ProbeBlock {
        probes,
        predicate,
        line,
        list_span: start + leading_ws..start + list.trim_end().len(),
    })
}

/// Split on commas outside parentheses (and outside strings, via the structure view).
fn split_probe_list(list: &str, structure: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut paren: usize = 0;
    let mut last = 0;
    for (k, &c) in structure.iter().enumerate() {
        match c {
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b',' if paren == 0 => {
                push_probe(&mut out, list.get(last..k));
                last = k + 1;
            }
            _ => {}
        }
    }
    push_probe(&mut out, list.get(last..));
    out
}

fn push_probe(out: &mut Vec<String>, part: Option<&str>) {
    let Some(part) = part else { return };
    // Collapse internal whitespace (a probe spec split across lines is still one spec).
    let probe = part.split_whitespace().collect::<Vec<_>>().join("");
    if !probe.is_empty() {
        out.push(probe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn probes(src: &str) -> Vec<Vec<String>> {
        probe_blocks(&mask(src)).into_iter().map(|b| b.probes).collect()
    }

    #[test]
    fn mask_keeps_length_and_newlines() {
        let src = "a // é{\n/* x\n{ */ b \"{é}\" c";
        let m = mask(src);
        assert_eq!(m.code.len(), src.len());
        assert_eq!(m.structure.len(), src.len());
        assert_eq!(m.code, "a       \n    \n     b \"{é}\" c");
        assert_eq!(m.structure_str(), "a       \n    \n     b \"xxxx\" c");
    }

    #[test]
    fn mask_handles_escapes_and_unterminated() {
        let m = mask(r#"printf("a\"{") { "#);
        assert_eq!(m.structure_str(), r#"printf("xxxx") { "#);
        let m = mask("\"never closed {\nk:f {");
        assert_eq!(m.structure_str(), format!("\"{}\nk:f {{", "x".repeat(14)));
        let m = mask("/* never closed {");
        assert_eq!(m.structure_str().trim(), "");
    }

    #[test]
    fn simple_blocks() {
        let src = "BEGIN { x }\nkprobe:vfs_read\n{\n@s[tid] = nsecs;\n}\nEND{}";
        assert_eq!(
            probes(src),
            vec![vec!["BEGIN"], vec!["kprobe:vfs_read"], vec!["END"]]
        );
    }

    #[test]
    fn multi_line_probe_list() {
        let src = "kprobe:a,\n  kprobe:b ,\n\tkretprobe:c\n{ }";
        assert_eq!(probes(src), vec![vec!["kprobe:a", "kprobe:b", "kretprobe:c"]]);
    }

    #[test]
    fn predicates_are_split_off() {
        let src = "kretprobe:vfs_read\n/@start[tid] && (x / 2) > 1/\n{ }";
        let blocks = probe_blocks(&mask(src));
        assert_eq!(blocks[0].probes, vec!["kretprobe:vfs_read"]);
        assert_eq!(blocks[0].predicate.as_deref(), Some("@start[tid] && (x / 2) > 1"));
    }

    #[test]
    fn uprobe_paths_are_not_predicates() {
        let src = "uprobe:/bin/bash:readline, uretprobe:/usr/lib/libc.so.6:malloc /pid == 1/ { }";
        let blocks = probe_blocks(&mask(src));
        assert_eq!(
            blocks[0].probes,
            vec!["uprobe:/bin/bash:readline", "uretprobe:/usr/lib/libc.so.6:malloc"]
        );
        assert_eq!(blocks[0].predicate.as_deref(), Some("pid == 1"));
    }

    #[test]
    fn braces_in_comments_and_strings_are_ignored() {
        let src =
            "// not { a probe\nBEGIN { printf(\"}{\"); /* } */ }\n/* { */ tracepoint:a:b { if (1) { } }";
        assert_eq!(probes(src), vec![vec!["BEGIN"], vec!["tracepoint:a:b"]]);
    }

    #[test]
    fn comment_inside_probe_list() {
        let src = "kprobe:a, // first\n/* second */ kprobe:b { }";
        assert_eq!(probes(src), vec![vec!["kprobe:a", "kprobe:b"]]);
    }

    #[test]
    fn preamble_items_are_not_probes() {
        let src = "#!/usr/bin/env bpftrace\n#include <linux/sched.h>\n\
                   struct foo { int a; };\nconfig = { max_map_keys = 10 }\n\
                   fn add(a: int): int { return a; }\nimport \"x\";\nlet @m = hash(10);\n\
                   BEGIN { }";
        assert_eq!(probes(src), vec![vec!["BEGIN"]]);
    }

    #[test]
    fn line_numbers() {
        let src = "// hdr\n\nBEGIN {}\n\n  kprobe:x,\nkprobe:y {}";
        let lines: Vec<_> = probe_blocks(&mask(src)).into_iter().map(|b| b.line).collect();
        assert_eq!(lines, vec![3, 5]);
    }

    #[test]
    fn regions_and_list_spans() {
        let src = "k:a, /* c */ k:b /x/ { printf(\"a\\\"b\"); }";
        let r = regions(src);
        let at = |needle: &str| src.find(needle).expect("needle");
        assert_eq!(r[at("/* c")], Region::Comment);
        assert_eq!(r[at("\"a")], Region::Quote);
        assert_eq!(r[at("a\\")], Region::StringBody);
        assert_eq!(r[at("b\")") + 1], Region::Quote);
        assert_eq!(r[at("printf")], Region::Code);

        let blocks = probe_blocks(&mask(src));
        assert_eq!(&src[blocks[0].list_span.clone()], "k:a, /* c */ k:b");
    }

    #[test]
    fn quoted_probe_parts_are_kept() {
        let src = "uprobe:/x:\"f,{g}\" { }";
        assert_eq!(probes(src), vec![vec!["uprobe:/x:\"f,{g}\""]]);
    }
}
