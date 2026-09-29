//! Event log ring buffer (spec §5.4): bounded, line-oriented, filterable.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

pub const DEFAULT_CAPACITY: usize = 10_000;
/// How far back a raw line looks for an identical line to count as a repeat.
const REPEAT_LOOKBACK: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    /// printf/time/cat/join/syscall/value output.
    Output,
    /// stderr lines and helper errors.
    Error,
    /// Unknown message types and non-JSON stdout, kept verbatim.
    Raw,
    /// bpfdeck's own notes: command line, stop requested, exit status.
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Monotonic, never reused; survives eviction (used to anchor a paused view).
    pub seq: u64,
    pub kind: LogKind,
    pub text: String,
    /// How many identical lines this line stands for (≥ 1): every repeat of an error
    /// (D-027), identical raw lines in a row.
    pub repeat: u64,
    /// Run time of the latest repeat.
    pub last: Duration,
    /// Output text not terminated by `\n` yet: the next output continues it.
    open: bool,
}

impl LogLine {
    /// The text with a `(×N, last mm:ss)` suffix for collapsed repeats.
    pub fn display(&self) -> String {
        if self.repeat > 1 {
            let s = self.last.as_secs();
            format!(
                "{} (×{}, last {:02}:{:02})",
                self.text,
                self.repeat,
                s / 60,
                s % 60
            )
        } else {
            self.text.clone()
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogBuffer {
    lines: VecDeque<LogLine>,
    capacity: usize,
    next_seq: u64,
    evicted: u64,
    /// Seq of the line holding each distinct error text.
    errors: HashMap<String, u64>,
    /// Run time stamped on new lines and repeats (`set_now`).
    now: Duration,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            capacity: capacity.max(1),
            next_seq: 0,
            evicted: 0,
            errors: HashMap::new(),
            now: Duration::ZERO,
        }
    }

    /// Run time for the lines pushed next.
    pub fn set_now(&mut self, now: Duration) {
        self.now = now;
    }

    /// The line with `seq`, if not evicted.
    fn by_seq(&mut self, seq: u64) -> Option<&mut LogLine> {
        let first = self.lines.front()?.seq;
        let i = usize::try_from(seq.checked_sub(first)?).ok()?;
        self.lines.get_mut(i)
    }

    /// Append one complete line (newlines inside are split into several lines).
    pub fn push_line(&mut self, kind: LogKind, text: &str) {
        self.close_open();
        for line in text.trim_end_matches('\n').split('\n') {
            // A helper error in a hot probe repeats thousands of times a second, usually
            // between program output. Each distinct error is shown once and counts its
            // repeats (D-027). Program output is never collapsed or skipped over: identical
            // printf lines are usually distinct events.
            if kind == LogKind::Error {
                let now = self.now;
                if let Some(&seq) = self.errors.get(line)
                    && let Some(same) = self.by_seq(seq)
                {
                    same.repeat += 1;
                    same.last = now;
                    continue;
                }
                self.push(kind, line, false);
                self.errors.insert(line.to_string(), self.next_seq - 1);
                if self.errors.len() > 2 * self.capacity {
                    let first = self.lines.front().map_or(0, |l| l.seq);
                    self.errors.retain(|_, seq| *seq >= first);
                }
                continue;
            }
            if kind == LogKind::Raw
                && let Some(same) = self
                    .lines
                    .iter_mut()
                    .rev()
                    .take(REPEAT_LOOKBACK)
                    .take_while(|l| matches!(l.kind, LogKind::Error | LogKind::Raw))
                    .find(|l| l.kind == kind && l.text == line)
            {
                same.repeat += 1;
                same.last = self.now;
                continue;
            }
            self.push(kind, line, false);
        }
    }

    /// Append program output that may contain several lines or end mid-line
    /// (`printf("a"); printf("b\n")` → one line `ab`).
    pub fn push_text(&mut self, kind: LogKind, text: &str) {
        let parts: Vec<&str> = text.split('\n').collect();
        let last = parts.len() - 1;
        for (i, part) in parts.into_iter().enumerate() {
            let terminated = i < last;
            if i == 0
                && let Some(open) = self.lines.back_mut().filter(|l| l.open && l.kind == kind)
            {
                open.text.push_str(part);
                open.open = !terminated;
                continue;
            }
            if !terminated && part.is_empty() {
                break;
            }
            self.close_open();
            self.push(kind, part, !terminated);
        }
    }

    fn close_open(&mut self) {
        if let Some(last) = self.lines.back_mut() {
            last.open = false;
        }
    }

    fn push(&mut self, kind: LogKind, text: &str, open: bool) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
            self.evicted += 1;
        }
        self.lines.push_back(LogLine {
            seq: self.next_seq,
            kind,
            text: text.to_string(),
            repeat: 1,
            last: self.now,
            open,
        });
        self.next_seq += 1;
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Lines dropped from the front because the buffer was full.
    pub fn evicted(&self) -> u64 {
        self.evicted
    }

    /// Lines containing `filter` (case-insensitive); all lines for an empty filter.
    pub fn matching(&self, filter: &str) -> Vec<&LogLine> {
        if filter.is_empty() {
            return self.lines.iter().collect();
        }
        let needle = filter.to_lowercase();
        self.lines
            .iter()
            .filter(|l| l.text.to_lowercase().contains(&needle))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn texts(log: &LogBuffer) -> Vec<(LogKind, &str)> {
        log.matching("")
            .into_iter()
            .map(|l| (l.kind, l.text.as_str()))
            .collect()
    }

    #[test]
    fn printf_fragments_are_joined_into_lines() {
        let mut log = LogBuffer::new(100);
        log.push_text(LogKind::Output, "Tracing... Hit Ctrl-C to end.\n");
        log.push_text(LogKind::Output, "a");
        log.push_text(LogKind::Output, "b");
        log.push_text(LogKind::Output, "c\nd\ne");
        log.push_text(LogKind::Output, "\n");
        log.push_text(LogKind::Output, "x\n\ny\n");
        assert_eq!(
            texts(&log),
            vec![
                (LogKind::Output, "Tracing... Hit Ctrl-C to end."),
                (LogKind::Output, "abc"),
                (LogKind::Output, "d"),
                (LogKind::Output, "e"),
                (LogKind::Output, "x"),
                (LogKind::Output, ""),
                (LogKind::Output, "y"),
            ]
        );
    }

    #[test]
    fn other_lines_close_an_open_output_line() {
        let mut log = LogBuffer::new(100);
        log.push_text(LogKind::Output, "partial");
        log.push_line(LogKind::Error, "ERROR: boom");
        log.push_text(LogKind::Output, " rest\n");
        assert_eq!(
            texts(&log),
            vec![
                (LogKind::Output, "partial"),
                (LogKind::Error, "ERROR: boom"),
                (LogKind::Output, " rest"),
            ]
        );
        log.push_line(LogKind::System, "two\nlines\n");
        assert_eq!(log.len(), 5);
    }

    #[test]
    fn ring_buffer_evicts_oldest_and_keeps_seq() {
        let mut log = LogBuffer::new(3);
        for i in 0..5 {
            log.push_line(LogKind::Output, &format!("line {i}"));
        }
        assert_eq!(log.len(), 3);
        assert_eq!(log.evicted(), 2);
        let seqs: Vec<u64> = log.matching("").iter().map(|l| l.seq).collect();
        assert_eq!(seqs, vec![2, 3, 4]);
    }

    #[test]
    fn each_error_is_shown_once_but_output_is_not_collapsed() {
        let mut log = LogBuffer::new(100);
        let e3 = "get_ns_current_pid_tgid: Invalid argument (line 3)";
        let e2 = "get_ns_current_pid_tgid: Invalid argument (line 2)";
        for _ in 0..3 {
            log.push_line(LogKind::Error, e3);
        }
        // Alternating call sites, and errors between program output (as with `pid` in a
        // container): still one line per distinct error.
        for i in 0..3 {
            log.set_now(Duration::from_secs(60 + i));
            log.push_line(LogKind::Error, e2);
            log.push_text(LogKind::Output, "0  wget\n");
            log.push_line(LogKind::Error, e3);
        }
        log.push_line(LogKind::System, "sys");
        log.push_line(LogKind::System, "sys");
        let shown: Vec<String> = log.matching("").iter().map(|l| l.display()).collect();
        assert_eq!(
            shown,
            vec![
                format!("{e3} (×6, last 01:02)"),
                format!("{e2} (×3, last 01:02)"),
                "0  wget".into(),
                "0  wget".into(),
                "0  wget".into(),
                "sys".into(),
                "sys".into(),
            ]
        );
    }

    #[test]
    fn an_evicted_error_starts_a_new_line() {
        let mut log = LogBuffer::new(2);
        log.push_line(LogKind::Error, "boom");
        log.push_line(LogKind::Output, "a");
        log.push_line(LogKind::Output, "b");
        log.push_line(LogKind::Error, "boom");
        log.push_line(LogKind::Error, "boom");
        let shown: Vec<String> = log.matching("").iter().map(|l| l.display()).collect();
        assert_eq!(shown, vec!["b".to_string(), "boom (×2, last 00:00)".to_string()]);
    }

    #[test]
    fn raw_lines_collapse_only_in_a_row() {
        let mut log = LogBuffer::new(100);
        for _ in 0..2 {
            log.push_line(LogKind::Raw, "odd");
            log.push_line(LogKind::Raw, "odd");
            log.push_text(LogKind::Output, "x\n");
        }
        let shown: Vec<String> = log.matching("").iter().map(|l| l.display()).collect();
        assert_eq!(
            shown,
            vec!["odd (×2, last 00:00)", "x", "odd (×2, last 00:00)", "x"]
        );
    }

    #[test]
    fn filter_is_case_insensitive_substring() {
        let mut log = LogBuffer::new(100);
        log.push_line(LogKind::Output, "PID 42 curl");
        log.push_line(LogKind::Output, "PID 7 sshd");
        log.push_line(LogKind::Error, "Curl failed");
        let hits: Vec<_> = log.matching("curl").iter().map(|l| l.text.as_str()).collect();
        assert_eq!(hits, vec!["PID 42 curl", "Curl failed"]);
        assert!(log.matching("nothing").is_empty());
    }
}
