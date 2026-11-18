//! Bounded output buffers for tool/agent execution streams.
//!
//! Command and tool output is appended as it streams and the buffer keeps at
//! most `capacity` bytes, so a `cargo build` spewing hundreds of megabytes
//! can never grow daemon RAM unboundedly (Roadmap Phase 1: bounded memory
//! ring buffers). The *tail* is retained because the interesting end of a
//! build log is its end.

use std::collections::VecDeque;

/// Circular byte buffer that retains the most recent `capacity` bytes.
#[derive(Debug, Clone)]
pub struct RingBuffer {
    inner: VecDeque<u8>,
    capacity: usize,
    dropped: u64,
}

impl RingBuffer {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "ring buffer capacity must be > 0");
        Self {
            inner: VecDeque::with_capacity(capacity.min(1024 * 1024)),
            capacity,
            dropped: 0,
        }
    }

    /// Append bytes, evicting the oldest ones beyond capacity.
    pub fn push(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.inner.len() == self.capacity {
                self.inner.pop_front();
                self.dropped += 1;
            }
            self.inner.push_back(b);
        }
    }

    /// Bytes evicted by [`push`](Self::push) so far.
    #[must_use]
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Retained bytes, prefixed with a truncation notice when data was lost.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.inner.len() + 32);
        if self.dropped > 0 {
            out.extend_from_slice(
                format!("[... {} bytes truncated ...]\n", self.dropped).as_bytes(),
            );
        }
        for chunk in self
            .inner
            .as_slices()
            .0
            .iter()
            .chain(self.inner.as_slices().1)
        {
            out.push(*chunk);
        }
        out
    }

    /// UTF-8 [`to_bytes`](Self::to_bytes), lossy at the head if a multi-byte
    /// character was split by eviction.
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.to_bytes()).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_tail_and_counts_drops() {
        let mut buf = RingBuffer::new(4);
        buf.push(b"abcdefg");
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.dropped_bytes(), 3);
        assert_eq!(buf.to_string_lossy(), "[... 3 bytes truncated ...]\ndefg");
    }

    #[test]
    fn under_capacity_is_exact() {
        let mut buf = RingBuffer::new(1024);
        buf.push(b"hello ");
        buf.push(b"world");
        assert_eq!(buf.dropped_bytes(), 0);
        assert_eq!(buf.to_string_lossy(), "hello world");
    }

    #[test]
    fn oversized_chunk_keeps_tail() {
        let mut buf = RingBuffer::new(2);
        buf.push(b"abcdefgh");
        assert_eq!(buf.to_string_lossy(), "[... 6 bytes truncated ...]\ngh");
    }

    #[test]
    fn empty_buffer_has_no_notice() {
        let buf = RingBuffer::new(8);
        assert!(buf.is_empty());
        assert!(buf.to_bytes().is_empty());
    }
}
