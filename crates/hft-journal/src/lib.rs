//! Bounded command journal: versioned accepted-command records handed from
//! the matching thread to a persistence consumer through the audited SPSC
//! ring. The matching side only enqueues — no storage calls, no locks, no
//! allocation after construction. Every record carries its sequence and an
//! FNV-1a checksum; the consumer verifies both before committing, so
//! corruption or truncation fails closed instead of persisting bad history.
#![forbid(unsafe_code)]

use core::fmt;
use hft_spsc::{Consumer, Producer, SpscQueue};
use hft_types::SequenceNumber;

/// Maximum journal payload: one wire frame.
pub const MAX_PAYLOAD: usize = 46;

/// Ring capacity for the persistence handoff (fixed, power of two).
pub const RING_CAPACITY: usize = 1024;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Batched FNV-1a over 62 bytes using 8 interleaved lanes to break the
/// serial multiply dependency chain (~4 cycles/byte → ~40 cycles total).
#[must_use]
pub fn record_checksum(sequence: u64, payload: &[u8]) -> u64 {
    let seq = sequence.to_be_bytes();
    let len_bytes = (payload.len() as u64).to_be_bytes();

    // 8 lanes, one per byte column of the u64 chunks.
    let mut lanes = [FNV_OFFSET; 8];

    // Mix sequence (8 bytes = 1 u64 chunk).
    for col in 0..8 {
        lanes[col] ^= u64::from(seq[col]);
        lanes[col] = lanes[col].wrapping_mul(FNV_PRIME);
    }

    // Mix payload in 5 full u64 chunks + 1 partial (6 bytes).
    for chunk in payload.chunks_exact(8) {
        for col in 0..8 {
            lanes[col] ^= u64::from(chunk[col]);
            lanes[col] = lanes[col].wrapping_mul(FNV_PRIME);
        }
    }
    let rem = &payload[(payload.len() / 8) * 8..];
    for (col, byte) in rem.iter().enumerate() {
        lanes[col] ^= u64::from(*byte);
        lanes[col] = lanes[col].wrapping_mul(FNV_PRIME);
    }

    // Mix length (8 bytes).
    for col in 0..8 {
        lanes[col] ^= u64::from(len_bytes[col]);
        lanes[col] = lanes[col].wrapping_mul(FNV_PRIME);
    }

    // Fold 8 lanes into one.
    let mut hash = FNV_OFFSET;
    for lane in lanes {
        hash ^= lane;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// One versioned accepted-command record. Exactly 64 bytes = 1 cache line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JournalRecord {
    pub sequence: SequenceNumber,
    pub checksum: u64,
    pub len: u16,
    pub payload: [u8; MAX_PAYLOAD],
}

impl JournalRecord {
    /// Builds a record and stamps its checksum.
    #[must_use]
    pub fn new(sequence: SequenceNumber, payload: &[u8]) -> Self {
        let len = payload.len().min(MAX_PAYLOAD);
        let mut stored = [0_u8; MAX_PAYLOAD];
        stored[..len].copy_from_slice(&payload[..len]);
        Self {
            checksum: record_checksum(sequence.0, &stored[..len]),
            sequence,
            len: u16::try_from(len).unwrap_or(u16::MAX),
            payload: stored,
        }
    }

    #[must_use]
    pub fn slice(&self) -> &[u8] {
        &self.payload[..Into::<usize>::into(self.len)]
    }

    /// Recomputes and compares the checksum over stored fields.
    #[must_use]
    pub fn verify(&self) -> bool {
        self.checksum == record_checksum(self.sequence.0, self.slice())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalError {
    /// The SPSC ring is full: backpressure is explicit, nothing is dropped.
    Saturated,
    /// Sequence space exhausted.
    SequenceOverflow,
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JournalError::Saturated => f.write_str("journal ring saturated"),
            JournalError::SequenceOverflow => f.write_str("journal sequence exhausted"),
        }
    }
}

/// Matching-thread side: stamps records and hands them to the ring.
/// Construction pins the starting sequence; `enqueue` is allocation-free.
pub struct JournalWriter<'queue> {
    producer: Producer<'queue, JournalRecord, RING_CAPACITY>,
    next_sequence: u64,
}

impl<'queue> JournalWriter<'queue> {
    /// Builds from one half of an already-split ring, so the writer and the
    /// persistence reader can coexist across phases or threads.
    #[must_use]
    pub fn from_producer(
        producer: Producer<'queue, JournalRecord, RING_CAPACITY>,
        first_sequence: u64,
    ) -> Self {
        Self {
            producer,
            next_sequence: first_sequence,
        }
    }

    /// Pins the first sequence the journal will stamp.
    #[must_use]
    pub fn new(
        queue: &'queue mut SpscQueue<JournalRecord, RING_CAPACITY>,
        first_sequence: u64,
    ) -> Self {
        let (producer, _) = queue.split();
        Self {
            producer,
            next_sequence: first_sequence,
        }
    }

    #[must_use]
    pub const fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Enqueues one accepted command. Allocation-free; fails closed on a
    /// saturated ring without consuming or corrupting state.
    ///
    /// # Errors
    ///
    /// [`JournalError::Saturated`] when the ring is full,
    /// [`JournalError::SequenceOverflow`] when the sequence space ends.
    pub fn enqueue(&mut self, payload: &[u8]) -> Result<SequenceNumber, JournalError> {
        let sequence = self.next_sequence;
        let record = JournalRecord::new(SequenceNumber(sequence), payload);
        match self.producer.try_push(record) {
            Ok(()) => {
                if sequence == u64::MAX {
                    return Err(JournalError::SequenceOverflow);
                }
                self.next_sequence = sequence + 1;
                Ok(SequenceNumber(sequence))
            }
            Err(same_record) => {
                debug_assert_eq!(same_record.sequence.0, sequence);
                debug_assert_eq!(same_record.checksum, record.checksum);
                Err(JournalError::Saturated)
            }
        }
    }
}

/// A verified record ready to be committed by the persistence layer.
#[derive(Clone, Copy, Debug)]
pub struct ParsedRecord {
    pub sequence: SequenceNumber,
    pub len: usize,
    pub payload: [u8; MAX_PAYLOAD],
}

impl ParsedRecord {
    #[must_use]
    pub fn slice(&self) -> &[u8] {
        &self.payload[..Into::<usize>::into(self.len)]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadError {
    /// No record pending right now.
    Empty,
    /// Stored checksum does not cover the stored bytes.
    ChecksumMismatch { sequence: SequenceNumber },
    /// Record breaks the strict in-order invariant.
    SequenceMismatch {
        expected: SequenceNumber,
        received: SequenceNumber,
    },
}

/// Persistence-consumer side: verifies records against sequence order and
/// checksum, then commits them into a bounded durable log. `commit` is the
/// explicit flush point — popped-but-uncommitted records are lost on crash,
/// which the fixtures use to pin exactly-once semantics across restarts.
pub struct JournalReader<'queue> {
    consumer: Consumer<'queue, JournalRecord, RING_CAPACITY>,
    log: std::vec::Vec<JournalRecord>,
    expected_sequence: u64,
}

impl<'queue> JournalReader<'queue> {
    /// Builds from one half of an already-split ring.
    #[must_use]
    pub fn from_consumer(
        consumer: Consumer<'queue, JournalRecord, RING_CAPACITY>,
        first_expected: u64,
    ) -> Self {
        Self {
            consumer,
            log: std::vec::Vec::with_capacity(RING_CAPACITY),
            expected_sequence: first_expected,
        }
    }

    #[must_use]
    pub fn new(
        queue: &'queue mut SpscQueue<JournalRecord, RING_CAPACITY>,
        first_expected: u64,
    ) -> Self {
        let (_, consumer) = queue.split();
        Self {
            consumer,
            log: std::vec::Vec::with_capacity(RING_CAPACITY),
            expected_sequence: first_expected,
        }
    }

    /// Pops and verifies exactly one record.
    ///
    /// # Errors
    ///
    /// [`ReadError::Empty`] when the ring has nothing pending,
    /// [`ReadError::ChecksumMismatch`] on corruption,
    /// [`ReadError::SequenceMismatch`] on a broken order invariant.
    pub fn read(&mut self) -> Result<ParsedRecord, ReadError> {
        let Some(record) = self.consumer.try_pop() else {
            return Err(ReadError::Empty);
        };
        if !record.verify() {
            return Err(ReadError::ChecksumMismatch {
                sequence: record.sequence,
            });
        }
        if record.sequence.0 != self.expected_sequence {
            return Err(ReadError::SequenceMismatch {
                expected: SequenceNumber(self.expected_sequence),
                received: record.sequence,
            });
        }
        self.expected_sequence += 1;
        Ok(ParsedRecord {
            sequence: record.sequence,
            len: usize::from(record.len),
            payload: record.payload,
        })
    }

    /// Commits a verified record to the durable log (the flush point).
    pub fn commit(&mut self, record: ParsedRecord) {
        self.log.push(JournalRecord {
            checksum: record_checksum(record.sequence.0, record.slice()),
            sequence: record.sequence,
            len: u16::try_from(record.len).unwrap_or(u16::MAX),
            payload: record.payload,
        });
    }

    /// Combined verify-and-commit convenience.
    ///
    /// # Errors
    ///
    /// Same as [`Self::read`].
    pub fn drain_one(&mut self) -> Result<(), ReadError> {
        let record = self.read()?;
        self.commit(record);
        Ok(())
    }

    /// The durable log as of the last commit.
    #[must_use]
    pub fn committed(&self) -> &[JournalRecord] {
        &self.log
    }

    #[must_use]
    pub const fn expected_sequence(&self) -> u64 {
        self.expected_sequence
    }
}
