//! Bounded retransmission buffer for session recovery.
//!
//! Accepted command frames are retained in FIFO order alongside their
//! sequence numbers. When the peer's last confirmed sequence advances, the
//! confirmed prefix is dropped. Recovery asks for `since(confirmed + 1)` and
//! receives exactly the still-retained suffix in order. Retention is finite:
//! once full, new admissions fail with [`RetainError::Full`] so exhaustion
//! is explicit rather than silently dropping history.
#![forbid(unsafe_code)]

use core::fmt;
use std::collections::VecDeque;

use hft_types::SequenceNumber;

/// Maximum retained payload size; matches the largest wire frame (new order).
pub const MAX_FRAME: usize = 46;

/// A retained frame awaiting confirmation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedFrame {
    pub sequence: SequenceNumber,
    pub bytes: [u8; MAX_FRAME],
    pub len: usize,
}

impl RetainedFrame {
    #[must_use]
    pub fn slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetainError {
    /// The buffer holds `capacity` frames and none were confirmed.
    Full,
    /// The sequence does not continue the retained window.
    NotInOrder {
        expected: SequenceNumber,
        received: SequenceNumber,
    },
}

impl fmt::Display for RetainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RetainError::Full => f.write_str("retransmission buffer full"),
            RetainError::NotInOrder { expected, received } => write!(
                f,
                "out-of-order retention: expected {expected:?}, received {received:?}"
            ),
        }
    }
}

/// FIFO retention window over consecutive sequence numbers.
#[derive(Debug)]
pub struct RetransmitBuffer {
    entries: VecDeque<RetainedFrame>,
    capacity: usize,
    next_sequence: u64,
}

impl RetransmitBuffer {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity),
            capacity,
            next_sequence: 1,
        }
    }

    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Sequence the next admitted frame must carry.
    #[must_use]
    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Retains one frame whose sequence continues the window exactly.
    ///
    /// # Errors
    ///
    /// [`RetainError::NotInOrder`] when the frame would leave a gap or
    /// duplicate history, [`RetainError::Full`] when retention is exhausted.
    pub fn retain(&mut self, sequence: SequenceNumber, bytes: &[u8]) -> Result<(), RetainError> {
        if bytes.len() > MAX_FRAME {
            return Err(RetainError::Full);
        }
        if sequence.0 != self.next_sequence {
            return Err(RetainError::NotInOrder {
                expected: SequenceNumber(self.next_sequence),
                received: sequence,
            });
        }
        if self.entries.len() == self.capacity {
            return Err(RetainError::Full);
        }
        let mut stored = [0_u8; MAX_FRAME];
        stored[..bytes.len()].copy_from_slice(bytes);
        self.entries.push_back(RetainedFrame {
            sequence,
            bytes: stored,
            len: bytes.len(),
        });
        self.next_sequence = sequence.0.checked_add(1).ok_or(RetainError::Full)?;
        Ok(())
    }

    /// Drops every frame up to and including `confirmed`; returns the number
    /// dropped. Frames above `confirmed` are retained for retransmission.
    pub fn confirm_through(&mut self, confirmed: u64) -> usize {
        let mut dropped = 0;
        while self
            .entries
            .front()
            .is_some_and(|frame| frame.sequence.0 <= confirmed)
        {
            self.entries.pop_front();
            dropped += 1;
        }
        dropped
    }

    /// Iterator over retained frames starting at `from`, in order. Frames
    /// below `from` were already delivered and are skipped.
    pub fn since(&self, from: u64) -> impl Iterator<Item = &RetainedFrame> {
        self.entries
            .iter()
            .filter(move |frame| frame.sequence.0 >= from)
    }

    /// Explicit exhaustion probe: how many more frames fit before
    /// [`RetainError::Full`].
    #[must_use]
    pub fn remaining_capacity(&self) -> usize {
        self.capacity - self.entries.len()
    }
}
