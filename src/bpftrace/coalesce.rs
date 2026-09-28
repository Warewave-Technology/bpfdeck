//! Coalescing of run events while the UI is behind (spec §6.5).
//!
//! Map/hist/stats/tseries snapshots replace each other, so only the latest per name is
//! kept. Text lines (printf, stderr, …) are all kept up to a cap; beyond it the oldest
//! are dropped and counted, never silently. The exit event always comes last, after the
//! exit-time dump. Pure: the executor feeds it and sends `take()` batches to the app.

use std::collections::{HashMap, VecDeque};

use super::json::OutputMsg;
use super::runner::RunEvent;

/// Events handed to the app in one message.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Batch {
    pub events: Vec<RunEvent>,
    /// Text lines dropped because the UI fell too far behind.
    pub dropped: u64,
}

#[derive(Debug)]
pub struct Coalescer {
    texts: VecDeque<RunEvent>,
    snapshots: Vec<RunEvent>,
    by_name: HashMap<String, usize>,
    exit: Option<RunEvent>,
    cap: usize,
    dropped: u64,
}

impl Coalescer {
    /// `cap`: most text lines held back at once (no point exceeding the log's size).
    pub fn new(cap: usize) -> Self {
        Self {
            texts: VecDeque::new(),
            snapshots: Vec::new(),
            by_name: HashMap::new(),
            exit: None,
            cap: cap.max(1),
            dropped: 0,
        }
    }

    pub fn push(&mut self, event: RunEvent) {
        match &event {
            RunEvent::Exited(_) => self.exit = Some(event),
            RunEvent::Output(
                OutputMsg::Map { name, .. }
                | OutputMsg::Hist { name, .. }
                | OutputMsg::Stats { name, .. }
                | OutputMsg::Tseries { name, .. },
            ) => match self.by_name.get(name) {
                Some(&i) => self.snapshots[i] = event,
                None => {
                    self.by_name.insert(name.clone(), self.snapshots.len());
                    self.snapshots.push(event);
                }
            },
            _ => {
                if self.texts.len() == self.cap {
                    self.texts.pop_front();
                    self.dropped += 1;
                }
                self.texts.push_back(event);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.texts.is_empty() && self.snapshots.is_empty() && self.exit.is_none() && self.dropped == 0
    }

    /// Everything held so far: text lines, then the latest snapshots, then the exit.
    pub fn take(&mut self) -> Batch {
        self.by_name.clear();
        let mut events: Vec<RunEvent> = self.texts.drain(..).collect();
        events.append(&mut self.snapshots);
        events.extend(self.exit.take());
        Batch {
            events,
            dropped: std::mem::take(&mut self.dropped),
        }
    }
}

/// A batch of one event, for tests of the app side.
#[cfg(test)]
pub fn one(event: RunEvent) -> Batch {
    Batch {
        events: vec![event],
        dropped: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bpftrace::json::{MapValue, TextKind};
    use crate::bpftrace::runner::RunExit;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn text(s: &str) -> RunEvent {
        RunEvent::Output(OutputMsg::Text {
            kind: TextKind::Printf,
            text: s.into(),
        })
    }

    fn map(name: &str, v: i64) -> RunEvent {
        RunEvent::Output(OutputMsg::Map {
            name: name.into(),
            value: MapValue::Scalar(json!(v)),
        })
    }

    fn exit() -> RunEvent {
        RunEvent::Exited(RunExit {
            code: Some(0),
            signal: None,
            forced: None,
            error: None,
        })
    }

    #[test]
    fn snapshots_coalesce_texts_do_not() {
        let mut c = Coalescer::new(100);
        assert!(c.is_empty());
        c.push(text("a"));
        c.push(map("@x", 1));
        c.push(map("@y", 1));
        c.push(text("b"));
        c.push(map("@x", 2));
        c.push(RunEvent::Stderr("warn".into()));
        c.push(exit());
        let batch = c.take();
        assert_eq!(
            batch.events,
            vec![
                text("a"),
                text("b"),
                RunEvent::Stderr("warn".into()),
                map("@x", 2),
                map("@y", 1),
                exit()
            ]
        );
        assert_eq!(batch.dropped, 0);
        assert!(c.is_empty());
        // Indices are reset between batches.
        c.push(map("@y", 5));
        assert_eq!(c.take().events, vec![map("@y", 5)]);
    }

    #[test]
    fn overflowing_texts_drop_oldest_and_count() {
        let mut c = Coalescer::new(3);
        for i in 0..10 {
            c.push(text(&i.to_string()));
        }
        assert!(!c.is_empty());
        let batch = c.take();
        assert_eq!(batch.events, vec![text("7"), text("8"), text("9")]);
        assert_eq!(batch.dropped, 7);
        assert_eq!(c.take().dropped, 0, "the drop count is reported once");
    }
}
