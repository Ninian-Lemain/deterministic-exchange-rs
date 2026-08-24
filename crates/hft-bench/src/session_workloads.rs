//! Session admission versus rejection against the gateway baseline: the
//! state machine sits in front of the matching core, and both paths are
//! timed end to end (session step plus gateway frame processing).

use crate::record::{BenchRecord, Extra};
use crate::{ALLOCATIONS, DEALLOCATIONS, analyze};
use hft_gateway::Gateway;
use hft_io::RxFrame;
use hft_risk::RiskEngine;
use hft_session::{SessionConfig, SessionEvent, SessionStateMachine};
use hft_types::{
    AccountId, InstrumentId, PriceTicks, Quantity, ReportBuffer, SequenceNumber, Side,
};
use hft_wire::encode_new_order;
use std::sync::atomic::Ordering;
use std::time::Instant;

const REPORTS: usize = 4;

fn gateway_fixture() -> Gateway<2, 8, 4, 4> {
    let mut risk = RiskEngine::<2, 8>::new();
    let limits = hft_risk::RiskLimits {
        max_quantity: Quantity(100),
        max_notional: 100_000,
        max_abs_position: Quantity(1_000_000),
        max_open_orders: 64,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(10_000),
    };
    risk.register_account(AccountId(1), limits)
        .expect("account one");
    risk.register_account(AccountId(2), limits)
        .expect("account two");
    Gateway::new(risk, InstrumentId(7))
}

fn frame(id: u64, account: u32, side: Side) -> [u8; 46] {
    encode_new_order(hft_types::NewOrder {
        time_in_force: hft_types::TimeInForce::Gtc,
        order_id: hft_types::OrderId(id),
        account_id: AccountId(account),
        instrument_id: InstrumentId(7),
        price: PriceTicks(100),
        quantity: Quantity(5),
        sequence: SequenceNumber(id),
        side,
    })
}

fn active_pair() -> (
    Gateway<2, 8, 4, 4>,
    SessionStateMachine,
    ReportBuffer<REPORTS>,
) {
    let mut session = SessionStateMachine::new(SessionConfig {
        logon_timeout_ticks: 0, // deadlines never fire mid-benchmark
        heartbeat_timeout_ticks: 0,
    });
    session.handle(SessionEvent::Connect, 0).expect("connect");
    session.handle(SessionEvent::LogonSent, 0).expect("logon");
    session
        .handle(
            SessionEvent::LogonAccepted {
                first_sequence: SequenceNumber(1),
            },
            0,
        )
        .expect("active");
    (gateway_fixture(), session, ReportBuffer::<REPORTS>::new())
}

/// Times one admitted command: session accepts the in-sequence frame and the
/// gateway processes it.
fn admit_step(
    gateway: &mut Gateway<2, 8, 4, 4>,
    session: &mut SessionStateMachine,
    reports: &mut ReportBuffer<REPORTS>,
    sequence: u64,
    bytes: &[u8; 46],
) -> u128 {
    let started = Instant::now();
    let _ = session.handle(
        SessionEvent::Command {
            sequence: SequenceNumber(sequence),
        },
        sequence,
    );
    let _ = gateway.process_frame(&RxFrame::from_bytes(bytes), reports);
    started.elapsed().as_nanos()
}

/// Times one refused command: a duplicate sequence is rejected by the
/// session before the gateway is touched.
fn reject_step(session: &mut SessionStateMachine, sequence: u64) -> u128 {
    let started = Instant::now();
    let _ = session.handle(
        SessionEvent::Command {
            sequence: SequenceNumber(sequence),
        },
        sequence,
    );
    started.elapsed().as_nanos()
}

fn run_cell<const ADMIT: bool>(samples: usize, scenario: &'static str, out: &mut Vec<BenchRecord>) {
    const WARMUP: usize = 32;
    if samples == 0 {
        return;
    }
    let (mut gateway, mut session, mut reports) = active_pair();
    let mut latencies = vec![0_u64; samples];
    let mut next_sequence = 1_u64;

    for sample_index in 0..WARMUP.saturating_sub(1).saturating_add(samples.min(1)) {
        if sample_index >= WARMUP {
            break;
        }
        let id = u64::try_from(sample_index + 1).unwrap_or(u64::MAX);
        let bytes = frame(
            id,
            u32::try_from(1 + sample_index % 2).unwrap_or(1),
            Side::Sell,
        );
        let _ = admit_step(&mut gateway, &mut session, &mut reports, id, &bytes);
        next_sequence = id + 1;
    }

    for (offset, sample) in latencies.iter_mut().enumerate() {
        // Alternate sides so the book keeps one resting order per level and
        // every admitted frame crosses or rests deterministically.
        let id = u64::try_from(WARMUP + offset + 1).unwrap_or(u64::MAX);
        let side = if offset % 2 == 0 {
            Side::Buy
        } else {
            Side::Sell
        };
        let bytes = frame(id, u32::try_from(1 + offset % 2).unwrap_or(1), side);
        let elapsed_ns = if ADMIT {
            let sequence = next_sequence;
            next_sequence += 1;
            admit_step(&mut gateway, &mut session, &mut reports, sequence, &bytes)
        } else {
            // Re-offer the previous sequence: always a duplicate refusal.
            reject_step(&mut session, next_sequence - 1)
        };
        *sample = u64::try_from(elapsed_ns).unwrap_or(u64::MAX);
    }

    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    let sample_count = latencies.len();
    let stats = analyze(&mut latencies);
    assert_eq!(
        ALLOCATIONS.load(Ordering::SeqCst),
        allocations_before,
        "{scenario} allocations"
    );
    assert_eq!(
        DEALLOCATIONS.load(Ordering::SeqCst),
        deallocations_before,
        "{scenario} deallocations"
    );

    let mut record = BenchRecord::new(
        "component",
        "session",
        scenario,
        &[("tif", Extra::Text("session"))],
    );
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
    out.push(record);
}

pub fn session_benchmark(samples: usize, out: &mut Vec<BenchRecord>) {
    run_cell::<true>(samples, "session_active_admission", out);
    run_cell::<false>(samples, "session_active_rejection", out);
}
