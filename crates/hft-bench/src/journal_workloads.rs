//! Journal benchmark cells: enqueue cost on the matching side, consumer
//! drain throughput, and saturation behavior — all allocation-gated.

use crate::record::{BenchRecord, Extra};
use crate::{ALLOCATIONS, DEALLOCATIONS, analyze};
use hft_journal::{JournalReader, JournalRecord, JournalWriter, RING_CAPACITY, ReadError};
use hft_spsc::SpscQueue;
use std::sync::atomic::Ordering;
use std::time::Instant;

const PAYLOAD: [u8; 46] = [0x4a; 46];

fn fill_record(record: &mut BenchRecord, latencies: &mut [u64]) {
    let sample_count = latencies.len();
    let stats = analyze(latencies);
    record.samples = sample_count;
    record.mean_ns = u64::try_from(stats.mean).unwrap_or(u64::MAX);
    record.p50_ns = stats.p50;
    record.p90_ns = stats.p90;
    record.p99_ns = stats.p99;
    record.p99_9_ns = stats.p99_9;
    record.max_ns = stats.max;
    record.ops_per_second = 1_000_000_000_u64
        .checked_div(record.mean_ns.max(1))
        .unwrap_or(0);
}

/// Matching-side enqueue: stamp checksum and push into the ring.
///
/// # Panics
///
/// Panics if a measured enqueue fails or an allocation gate trips.
pub fn journal_benchmark(samples: usize, out: &mut Vec<BenchRecord>) {
    if samples == 0 {
        return;
    }
    let mut queue = SpscQueue::<JournalRecord, RING_CAPACITY>::try_new().expect("ring");
    let (producer, mut consumer) = queue.split();
    let mut writer = JournalWriter::from_producer(producer, 1);

    // Warm-up: fill half the ring untimed, then drain it so the measured
    // window starts with a known-empty ring.
    for _ in 1..=(RING_CAPACITY / 2) as u64 {
        writer.enqueue(&PAYLOAD).expect("warm-up fits");
        let _ = consumer.try_pop();
    }

    let mut latencies = vec![0_u64; samples];
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    let mut checksum = 0_u64;
    let mut drained = 0_u64;
    for (offset, sample) in latencies.iter_mut().enumerate() {
        // Steady state: one enqueue timed, one drain untimed so the ring
        // never fills across the run.
        let started = Instant::now();
        let sequence = writer.enqueue(&PAYLOAD).expect("measured enqueue fits");
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if consumer.try_pop().is_some() {
            drained += 1;
        }
        checksum_fold(&mut checksum, sequence.0, offset);
    }
    assert_gate(
        (allocations_before, deallocations_before),
        "journal_enqueue",
    );
    drain_ring(&mut consumer);

    let mut record = BenchRecord::new(
        "component",
        "journal",
        "journal_enqueue",
        &[("tif", Extra::Text("journal"))],
    );
    fill_record(&mut record, &mut latencies);
    record.checksum = checksum ^ drained.rotate_left(1);
    out.push(record);
}
const BATCH: usize = 8;

/// Persistence-side drain throughput over a pre-filled ring.
///
/// # Panics
///
/// Panics if a drain fails or an allocation gate trips.
pub fn journal_drain_benchmark(samples: usize, out: &mut Vec<BenchRecord>) {
    if samples == 0 {
        return;
    }
    let mut queue = SpscQueue::<JournalRecord, RING_CAPACITY>::try_new().expect("ring");
    {
        let (producer, _) = queue.split();
        let mut writer = hft_journal::JournalWriter::from_producer(producer, 1);
        for _ in 0..RING_CAPACITY {
            writer.enqueue(&PAYLOAD).expect("prefill fits");
        }
    }

    let mut reader = JournalReader::from_consumer(split_consumer(&mut queue), 1);
    // Warm-up: verify-and-commit the first 32 records untimed.
    for _ in 0..32 {
        reader.drain_one().expect("warm-up drains");
    }

    let batches = samples.min(RING_CAPACITY.saturating_sub(32) / BATCH).max(1);
    let mut latencies = vec![0_u64; batches];
    let mut committed_total = 0_usize;
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    for sample in &mut latencies {
        let started = Instant::now();
        for _ in 0..BATCH {
            match reader.drain_one() {
                Ok(()) => committed_total += 1,
                Err(ReadError::Empty) => break,
                Err(other) => panic!("drain failed: {other:?}"),
            }
        }
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    }
    assert_gate((allocations_before, deallocations_before), "journal_drain");

    let mut record = BenchRecord::new(
        "component",
        "journal",
        "journal_drain",
        &[("tif", Extra::Text("journal"))],
    );
    fill_record(&mut record, &mut latencies);
    record.checksum = u64::try_from(committed_total).unwrap_or(u64::MAX);
    out.push(record);
}

/// Saturation: fill to capacity, assert explicit refusal, drain, continue.
#[test]
fn saturation_smoke() {
    // Under a Loom build the ring's primitives panic outside a model; the
    // Loom unit tests in hft-spsc cover this path there.
    if hft_spsc::IS_LOOM_BUILD {
        return;
    }
    let mut queue = SpscQueue::<JournalRecord, RING_CAPACITY>::try_new().unwrap();
    let (producer, mut consumer) = queue.split();
    let mut writer = hft_journal::JournalWriter::from_producer(producer, 1);
    for _ in 1..=RING_CAPACITY as u64 {
        writer.enqueue(&[0_u8; 46]).unwrap();
    }
    assert!(matches!(
        writer.enqueue(&[1_u8; 46]),
        Err(hft_journal::JournalError::Saturated)
    ));
    while consumer.try_pop().is_some() {}
    writer.enqueue(&[2_u8; 46]).unwrap();
}

fn checksum_fold(checksum: &mut u64, sequence: u64, offset: usize) {
    *checksum ^= sequence.wrapping_shr(u32::try_from(offset % 64).unwrap_or(0));
}

fn drain_ring(consumer: &mut hft_spsc::Consumer<'_, JournalRecord, RING_CAPACITY>) {
    while consumer.try_pop().is_some() {}
}

fn assert_gate(before: (u64, u64), context: &'static str) {
    assert_eq!(
        ALLOCATIONS.load(Ordering::SeqCst),
        before.0,
        "{context} allocations"
    );
    assert_eq!(
        DEALLOCATIONS.load(Ordering::SeqCst),
        before.1,
        "{context} deallocations"
    );
}

fn split_consumer(
    queue: &mut SpscQueue<JournalRecord, RING_CAPACITY>,
) -> hft_spsc::Consumer<'_, JournalRecord, RING_CAPACITY> {
    let (_, consumer) = queue.split();
    consumer
}
