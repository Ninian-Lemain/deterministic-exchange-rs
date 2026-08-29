//! Snapshot and journal-tail recovery benchmark cells.

use crate::record::{BenchRecord, Extra};
use crate::{ALLOCATIONS, DEALLOCATIONS, analyze};
use hft_gateway::{Gateway, GatewayError};
use hft_io::RxFrame;
use hft_journal::JournalRecord;
use hft_recovery::{decode_snapshot, encode_snapshot, recover_snapshot_and_tail};
use hft_risk::{RiskEngine, RiskLimits};
use hft_types::{
    AccountId, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, ReportBuffer, SequenceNumber,
    Side, TimeInForce,
};
use hft_wire::encode_new_order;
use std::hint::black_box;
use std::mem::size_of_val;
use std::sync::atomic::Ordering;
use std::time::Instant;

const ACCOUNTS: usize = 2;
const RISK_ORDERS: usize = 32;
const LEVELS: usize = 16;
const ORDERS_PER_LEVEL: usize = 8;
const REPORTS: usize = 8;
const SNAPSHOT_COMMANDS: u64 = 8;
const TAIL_COMMANDS: u64 = 8;

type BenchGateway = Gateway<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>;

/// Measures canonical encoding, verified restore, and restore plus tail replay.
///
/// # Panics
///
/// Panics if deterministic fixture construction or a recovery operation fails.
pub fn recovery_benchmarks(samples: usize, out: &mut Vec<BenchRecord>) {
    if samples == 0 {
        return;
    }

    let gateway = snapshot_gateway();
    let snapshot = encode_snapshot(&gateway, SNAPSHOT_COMMANDS).expect("snapshot fixture");
    let tail = journal_tail();
    let state_bytes = size_of_val(&gateway);
    let snapshot_bytes = snapshot.bytes().len();

    for _ in 0..16 {
        black_box(encode_snapshot(&gateway, SNAPSHOT_COMMANDS).expect("encode warmup"));
        black_box(
            decode_snapshot::<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>(snapshot.bytes())
                .expect("decode warmup"),
        );
        black_box(
            recover_snapshot_and_tail::<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL, REPORTS>(
                snapshot.bytes(),
                &tail,
            )
            .expect("replay warmup"),
        );
    }

    let mut latencies = vec![0_u64; samples];
    let before = allocation_counts();
    let mut checksum = 0_u64;
    for sample in &mut latencies {
        let started = Instant::now();
        let encoded = encode_snapshot(black_box(&gateway), SNAPSHOT_COMMANDS).expect("encode");
        *sample = elapsed_ns(started);
        checksum ^= u64::try_from(encoded.bytes().len()).unwrap_or(u64::MAX);
        black_box(encoded);
    }
    push_record(
        out,
        "canonical_snapshot_encode",
        state_bytes,
        snapshot_bytes,
        SNAPSHOT_COMMANDS,
        &mut latencies,
        before,
        checksum,
    );

    let before = allocation_counts();
    let mut checksum = 0_u64;
    for sample in &mut latencies {
        let started = Instant::now();
        let decoded = decode_snapshot::<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>(
            black_box(snapshot.bytes()),
        )
        .expect("verified restore");
        *sample = elapsed_ns(started);
        checksum ^= decoded.applied_sequence ^ u64::from(decoded.digest[0]);
        black_box(decoded);
    }
    push_record(
        out,
        "verified_snapshot_restore",
        state_bytes,
        snapshot_bytes,
        SNAPSHOT_COMMANDS,
        &mut latencies,
        before,
        checksum,
    );

    let before = allocation_counts();
    let mut checksum = 0_u64;
    for sample in &mut latencies {
        let started = Instant::now();
        let recovered =
            recover_snapshot_and_tail::<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL, REPORTS>(
                black_box(snapshot.bytes()),
                black_box(&tail),
            )
            .expect("snapshot and tail replay");
        *sample = elapsed_ns(started);
        checksum ^= recovered.export_state().expected_sequence.0;
        black_box(recovered);
    }
    push_replay_record(
        out,
        state_bytes,
        snapshot_bytes,
        tail.len(),
        SNAPSHOT_COMMANDS + TAIL_COMMANDS,
        &mut latencies,
        before,
        checksum,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_record(
    out: &mut Vec<BenchRecord>,
    scenario: &'static str,
    state_bytes: usize,
    snapshot_bytes: usize,
    commands: u64,
    latencies: &mut [u64],
    before: (u64, u64),
    checksum: u64,
) {
    let mut record = BenchRecord::new(
        "component",
        "recovery",
        scenario,
        &[
            ("state_bytes", Extra::U64(as_u64(state_bytes))),
            ("snapshot_bytes", Extra::U64(as_u64(snapshot_bytes))),
            ("commands", Extra::U64(commands)),
        ],
    );
    fill_record(&mut record, latencies, before, checksum);
    out.push(record);
}

#[allow(clippy::too_many_arguments)]
fn push_replay_record(
    out: &mut Vec<BenchRecord>,
    state_bytes: usize,
    snapshot_bytes: usize,
    tail_bytes: usize,
    commands: u64,
    latencies: &mut [u64],
    before: (u64, u64),
    checksum: u64,
) {
    let mut record = BenchRecord::new(
        "component",
        "recovery",
        "snapshot_tail_replay",
        &[
            ("state_bytes", Extra::U64(as_u64(state_bytes))),
            ("snapshot_bytes", Extra::U64(as_u64(snapshot_bytes))),
            ("tail_bytes", Extra::U64(as_u64(tail_bytes))),
            ("commands", Extra::U64(commands)),
        ],
    );
    fill_record(&mut record, latencies, before, checksum);
    out.push(record);
}

fn fill_record(record: &mut BenchRecord, latencies: &mut [u64], before: (u64, u64), checksum: u64) {
    let after = allocation_counts();
    let stats = analyze(latencies);
    record.samples = latencies.len();
    record.mean_ns = u64::try_from(stats.mean).unwrap_or(u64::MAX);
    record.p50_ns = stats.p50;
    record.p90_ns = stats.p90;
    record.p99_ns = stats.p99;
    record.p99_9_ns = stats.p99_9;
    record.max_ns = stats.max;
    record.ops_per_second = 1_000_000_000_u64
        .checked_div(record.mean_ns.max(1))
        .unwrap_or(0);
    record.allocations = after.0.saturating_sub(before.0);
    record.deallocations = after.1.saturating_sub(before.1);
    record.checksum = checksum;
}

fn snapshot_gateway() -> BenchGateway {
    let mut gateway = gateway();
    for sequence in 1..=SNAPSHOT_COMMANDS {
        apply(&mut gateway, &order(sequence, sequence)).expect("snapshot command");
    }
    gateway
}

fn journal_tail() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(as_usize(TAIL_COMMANDS) * hft_journal::RECORD_SIZE);
    for offset in 1..=TAIL_COMMANDS {
        let sequence = SNAPSHOT_COMMANDS + offset;
        let frame = order(sequence, sequence);
        let record = JournalRecord::new(SequenceNumber(sequence), &frame).expect("journal record");
        bytes.extend_from_slice(&record.encode());
    }
    bytes
}

fn gateway() -> BenchGateway {
    let limits = RiskLimits {
        max_quantity: Quantity(10),
        max_notional: 10_000,
        max_abs_position: Quantity(100),
        max_open_orders: 32,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    };
    let mut risk = RiskEngine::new();
    risk.register_account(AccountId(1), limits)
        .expect("benchmark account");
    Gateway::new(risk, InstrumentId(7))
}

fn order(id: u64, sequence: u64) -> [u8; 46] {
    encode_new_order(NewOrder {
        order_id: OrderId(id),
        account_id: AccountId(1),
        instrument_id: InstrumentId(7),
        price: PriceTicks(100 + i64::try_from(id).unwrap_or(i64::MAX)),
        quantity: Quantity(1),
        sequence: SequenceNumber(sequence),
        side: Side::Sell,
        time_in_force: TimeInForce::Gtc,
    })
}

fn apply(gateway: &mut BenchGateway, bytes: &[u8]) -> Result<(), GatewayError> {
    let mut reports = ReportBuffer::<REPORTS>::new();
    gateway
        .process_frame(&RxFrame::from_bytes(bytes), &mut reports)
        .map(|_| ())
}

fn allocation_counts() -> (u64, u64) {
    (
        ALLOCATIONS.load(Ordering::SeqCst),
        DEALLOCATIONS.load(Ordering::SeqCst),
    )
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn as_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}
