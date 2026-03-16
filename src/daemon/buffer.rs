use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub stream: Stream,
    pub line: String,
}

pub struct LogBuffer {
    entries: VecDeque<LogEntry>,
    capacity: usize,
    seq_counter: u64,       // private; exposed via peek_next_seq()
    pub dropped: u64,
    pub total_bytes: u64,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
            seq_counter: 0,
            dropped: 0,
            total_bytes: 0,
        }
    }

    pub fn push(&mut self, stream: Stream, line: String, ts: DateTime<Utc>) -> u64 {
        let seq = self.seq_counter;
        self.seq_counter += 1;
        if stream != Stream::System {
            self.total_bytes += line.len() as u64 + 1;
        }
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
            self.dropped += 1;
        }
        self.entries.push_back(LogEntry { seq, ts, stream, line });
        seq
    }

    /// Returns last `limit` entries (newest).
    pub fn tail(&self, limit: usize) -> Vec<LogEntry> {
        let skip = self.entries.len().saturating_sub(limit);
        self.entries.iter().skip(skip).cloned().collect()
    }

    /// Returns first `limit` entries currently in the buffer (oldest).
    pub fn head(&self, limit: usize) -> Vec<LogEntry> {
        self.entries.iter().take(limit).cloned().collect()
    }

    /// Returns up to `limit` entries with seq >= n.
    pub fn since_seq(&self, n: u64, limit: usize) -> Vec<LogEntry> {
        self.entries.iter().filter(|e| e.seq >= n).take(limit).cloned().collect()
    }

    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    /// The seq number that the next push() call will assign.
    pub fn peek_next_seq(&self) -> u64 { self.seq_counter }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn ts() -> chrono::DateTime<Utc> { Utc::now() }

    #[test]
    fn push_increments_seq() {
        let mut buf = LogBuffer::new(100);
        let s0 = buf.push(Stream::Stdout, "line0".into(), ts());
        let s1 = buf.push(Stream::Stdout, "line1".into(), ts());
        assert_eq!(s0, 0);
        assert_eq!(s1, 1);
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn push_evicts_oldest_when_full() {
        let mut buf = LogBuffer::new(3);
        buf.push(Stream::Stdout, "a".into(), ts());
        buf.push(Stream::Stdout, "b".into(), ts());
        buf.push(Stream::Stdout, "c".into(), ts());
        buf.push(Stream::Stdout, "d".into(), ts()); // evicts "a"
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.dropped, 1);
        let entries = buf.tail(3);
        assert_eq!(entries[0].line, "b");
        assert_eq!(entries[2].line, "d");
    }

    #[test]
    fn tail_returns_newest_n() {
        let mut buf = LogBuffer::new(100);
        for i in 0..10u64 { buf.push(Stream::Stdout, format!("line{i}"), ts()); }
        let t = buf.tail(3);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].line, "line7");
        assert_eq!(t[2].line, "line9");
    }

    #[test]
    fn head_returns_oldest_n() {
        let mut buf = LogBuffer::new(100);
        for i in 0..10u64 { buf.push(Stream::Stdout, format!("line{i}"), ts()); }
        let h = buf.head(3);
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].line, "line0");
        assert_eq!(h[2].line, "line2");
    }

    #[test]
    fn since_seq_filters_correctly() {
        let mut buf = LogBuffer::new(100);
        for i in 0..10u64 { buf.push(Stream::Stdout, format!("line{i}"), ts()); }
        let entries = buf.since_seq(5, 100);
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].seq, 5);
        assert_eq!(entries[4].seq, 9);
    }

    #[test]
    fn since_seq_respects_limit() {
        let mut buf = LogBuffer::new(100);
        for i in 0..10u64 { buf.push(Stream::Stdout, format!("line{i}"), ts()); }
        let entries = buf.since_seq(0, 3);
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn total_bytes_counts_stdout_stderr_not_system() {
        let mut buf = LogBuffer::new(100);
        buf.push(Stream::Stdout, "hello".into(), ts()); // 5+1=6
        buf.push(Stream::Stderr, "world".into(), ts()); // 5+1=6
        buf.push(Stream::System, "diag".into(),  ts()); // ignored
        assert_eq!(buf.total_bytes, 12);
    }

    #[test]
    fn peek_next_seq_advances() {
        let mut buf = LogBuffer::new(100);
        assert_eq!(buf.peek_next_seq(), 0);
        buf.push(Stream::Stdout, "x".into(), ts());
        assert_eq!(buf.peek_next_seq(), 1);
    }
}
