//! Journal invariants: records occur exactly once and in order; corruption,
//! truncation, and saturation fail closed; a crash between ring handoff and
//! commit loses only the uncommitted suffix, and restart resumes with no gap
//! and no duplicate.
//!
//! Skipped on Loom builds: the ring's primitives panic outside a model; the
//! Loom unit tests in hft-spsc cover the algorithm there.

use hft_journal::{
    JournalError, JournalReader, JournalRecord, JournalWriter, RING_CAPACITY, ReadError,
};
use hft_spsc::SpscQueue;
use hft_types::SequenceNumber;

fn payload(id: u64) -> [u8; 46] {
    let mut bytes = [0_u8; 46];
    bytes[..8].copy_from_slice(&id.to_be_bytes());
    bytes[8] = 0x4a;
    let tail = id.wrapping_mul(0x5851_5f64).to_be_bytes();
    bytes[40..46].copy_from_slice(&tail[2..]);
    bytes
}

fn plant_records(queue: &mut SpscQueue<JournalRecord, RING_CAPACITY>, records: &[JournalRecord]) {
    let (mut producer, _) = queue.split();
    for record in records {
        producer.try_push(*record).expect("plant fits");
    }
}

#[test]
fn records_commit_once_in_order_with_intact_payloads() {
    if hft_spsc::IS_LOOM_BUILD {
        eprintln!("skipped: loom build");
        return;
    }
    let mut queue = SpscQueue::<JournalRecord, RING_CAPACITY>::try_new().unwrap();
    let first_sequence = 1_u64;
    {
        let mut writer = JournalWriter::new(&mut queue, first_sequence);
        for id in 1..=100_u64 {
            writer.enqueue(&payload(id)).unwrap();
        }
    }

    let mut reader = JournalReader::new(&mut queue, first_sequence);
    for id in 1..=100_u64 {
        let record = reader.read().unwrap();
        assert_eq!(record.sequence.0, id);
        assert_eq!(record.slice(), &payload(id)[..]);
        reader.commit(record);
    }
    assert!(matches!(reader.read(), Err(ReadError::Empty)));

    for (offset, record) in reader.committed().iter().enumerate() {
        assert_eq!(record.sequence.0, offset as u64 + 1);
        assert_eq!(record.slice(), &payload(offset as u64 + 1)[..]);
        assert!(record.verify());
    }
}

#[test]
fn corruption_fails_closed_without_committing() {
    if hft_spsc::IS_LOOM_BUILD {
        eprintln!("skipped: loom build");
        return;
    }
    let mut queue = SpscQueue::<JournalRecord, RING_CAPACITY>::try_new().unwrap();

    let good1 = JournalRecord::new(SequenceNumber(1), &payload(1));
    let mut bad2 = JournalRecord::new(SequenceNumber(2), &payload(2));
    bad2.checksum ^= 1;
    let good3 = JournalRecord::new(SequenceNumber(3), &payload(3));
    plant_records(&mut queue, &[good1, bad2, good3]);

    let mut reader = JournalReader::new(&mut queue, 1);
    let record = reader.read().unwrap();
    reader.commit(record);

    match reader.read() {
        Err(ReadError::ChecksumMismatch { sequence }) => {
            assert_eq!(sequence.0, 2);
        }
        other => panic!("expected checksum mismatch, got {other:?}"),
    }
    assert_eq!(reader.committed().len(), 1);
}

#[test]
fn out_of_order_record_fails_closed() {
    if hft_spsc::IS_LOOM_BUILD {
        eprintln!("skipped: loom build");
        return;
    }
    let mut queue = SpscQueue::<JournalRecord, RING_CAPACITY>::try_new().unwrap();
    let wrong = JournalRecord::new(SequenceNumber(7), &payload(7));
    plant_records(&mut queue, &[wrong]);

    let mut reader = JournalReader::new(&mut queue, 1);
    match reader.read() {
        Err(ReadError::SequenceMismatch { expected, received }) => {
            assert_eq!(expected.0, 1);
            assert_eq!(received.0, 7);
        }
        other => panic!("expected sequence mismatch, got {other:?}"),
    }
}

#[test]
fn saturation_is_explicit_and_recovery_continues_after_drain() {
    if hft_spsc::IS_LOOM_BUILD {
        eprintln!("skipped: loom build");
        return;
    }
    let mut queue = SpscQueue::<JournalRecord, RING_CAPACITY>::try_new().unwrap();
    let (producer, consumer) = queue.split();
    let mut writer = JournalWriter::from_producer(producer, 1);
    let mut reader = JournalReader::from_consumer(consumer, 1);

    for _ in 1..=RING_CAPACITY as u64 {
        writer.enqueue(&payload(1)).expect("ring fills exactly");
    }
    assert_eq!(
        writer.enqueue(&payload(RING_CAPACITY as u64 + 1)),
        Err(JournalError::Saturated),
        "overflow must be refused, not dropped"
    );

    for _ in 1..=(RING_CAPACITY / 2) as u64 {
        reader.drain_one().unwrap();
    }
    writer.enqueue(&payload(RING_CAPACITY as u64 + 1)).unwrap();

    let resume = (RING_CAPACITY / 2 + 1) as u64;
    for id in resume..=(RING_CAPACITY as u64 + 1) {
        let record = reader.read().unwrap();
        assert_eq!(record.sequence.0, id);
        reader.commit(record);
    }
    assert_eq!(
        reader.committed().len(),
        RING_CAPACITY / 2 + (RING_CAPACITY / 2) + 1
    );
}

#[test]
fn crash_between_handoff_and_commit_loses_only_uncommitted_suffix() {
    if hft_spsc::IS_LOOM_BUILD {
        eprintln!("skipped: loom build");
        return;
    }

    // ---- first life ----
    let committed_before_crash;
    {
        let mut queue = SpscQueue::<JournalRecord, RING_CAPACITY>::try_new().unwrap();
        {
            let mut writer = JournalWriter::new(&mut queue, 1);
            for id in 1..=5_u64 {
                writer.enqueue(&payload(id)).unwrap();
            }
        }
        let mut reader = JournalReader::new(&mut queue, 1);
        for _ in 1..=3 {
            let record = reader.read().unwrap();
            reader.commit(record);
        }
        committed_before_crash = reader.committed().to_vec();
    }
    assert_eq!(committed_before_crash.len(), 3);

    // ---- restart ----
    let last_committed = committed_before_crash.len() as u64;
    let mut queue = SpscQueue::<JournalRecord, RING_CAPACITY>::try_new().unwrap();
    {
        let mut writer = JournalWriter::new(&mut queue, last_committed + 1);
        for seq in (last_committed + 1)..=5_u64 {
            writer.enqueue(&payload(seq)).unwrap();
        }
    }
    let mut reader = JournalReader::new(&mut queue, last_committed + 1);
    for seq in (last_committed + 1)..=5_u64 {
        let record = reader.read().unwrap();
        assert_eq!(record.sequence.0, seq);
        reader.commit(record);
    }

    // Merged history: pre-crash commits plus post-restart commits give
    // the full sequence, each exactly once, payloads intact.
    let mut seen = std::collections::HashSet::new();
    let merged: Vec<&JournalRecord> = committed_before_crash
        .iter()
        .chain(reader.committed().iter())
        .collect();
    for (offset, record) in merged.iter().enumerate() {
        let expected_id = offset as u64 + 1;
        assert_eq!(record.sequence.0, expected_id, "no gaps");
        assert!(seen.insert(record.sequence.0), "no duplicates");
        assert_eq!(record.slice(), &payload(expected_id)[..]);
    }
    assert_eq!(seen.len(), 5);
}
