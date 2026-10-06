//! Session admission versus rejection against the gateway baseline: the
//! state machine sits in front of the matching core, and both paths are
//! timed end to end (session step plus gateway frame processing).

use crate::record::{BenchRecord, Extra};
use crate::{ALLOCATIONS, DEALLOCATIONS, analyze};
use hft_gateway::{Gateway, GatewayOutcome};
use hft_io::RxFrame;
use hft_risk::RiskEngine;
use hft_session::{SessionConfig, SessionError, SessionEvent, SessionState, SessionStateMachine};
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
    let admitted = session.handle(
        SessionEvent::Command {
            sequence: SequenceNumber(sequence),
        },
        sequence,
    );
    let outcome = if admitted.is_ok() {
        Some(gateway.process_frame(&RxFrame::from_bytes(bytes), reports))
    } else {
        None
    };
    let elapsed = started.elapsed().as_nanos();
    assert_eq!(
        admitted.expect("session admission").state,
        SessionState::Active
    );
    let Some(Ok(GatewayOutcome::NewOrder(summary))) = outcome else {
        panic!("session fixture must admit every gateway order: {outcome:?}");
    };
    let crossing = sequence % 2 == 0;
    assert_eq!(
        summary.filled_quantity,
        Quantity(if crossing { 5 } else { 0 })
    );
    assert_eq!(
        summary.resting_quantity,
        Quantity(if crossing { 0 } else { 5 })
    );
    assert_eq!(reports.len(), usize::from(crossing));
    assert_eq!(summary.discarded_quantity, Quantity(0));
    assert_eq!(summary.report_count, usize::from(crossing));
    for report in reports.iter() {
        assert_eq!(report.maker_order_id.0, sequence - 1);
        assert_eq!(report.taker_order_id.0, sequence);
        assert_eq!(report.sequence, SequenceNumber(sequence));
        assert_eq!(report.quantity, Quantity(5));
        assert_eq!(report.price, PriceTicks(100));
        assert_eq!(report.instrument_id, InstrumentId(7));
    }
    assert_eq!(gateway.expected_sequence(), session.expected_sequence());
    elapsed
}

/// Times one refused command: a duplicate sequence is rejected by the
/// session before the gateway is touched.
fn reject_step(session: &mut SessionStateMachine, sequence: u64) -> u128 {
    let started = Instant::now();
    let refused = session.handle(
        SessionEvent::Command {
            sequence: SequenceNumber(sequence),
        },
        sequence,
    );
    let elapsed = started.elapsed().as_nanos();
    assert_eq!(
        refused,
        Err(SessionError::DuplicateSequence {
            received: SequenceNumber(sequence),
            expected: SequenceNumber(sequence + 1),
        })
    );
    assert_eq!(session.expected_sequence(), SequenceNumber(sequence + 1));
    assert_eq!(session.state(), SessionState::Active);
    elapsed
}

fn run_cell<const ADMIT: bool>(samples: usize, scenario: &'static str, out: &mut Vec<BenchRecord>) {
    const WARMUP: usize = 32;
    if samples == 0 {
        return;
    }
    let (mut gateway, mut session, mut reports) = active_pair();
    let mut latencies = vec![0_u64; samples];
    for sample_index in 0..WARMUP {
        let id = u64::try_from(sample_index + 1).unwrap_or(u64::MAX);
        let bytes = workload_frame(id);
        let _ = admit_step(&mut gateway, &mut session, &mut reports, id, &bytes);
    }

    let initial_digest = gateway.stable_digest();
    let initial_sequence = session.expected_sequence();
    let initial_deadline = session.deadline();
    let mut checksum = initial_digest;
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);

    for (offset, sample) in latencies.iter_mut().enumerate() {
        // Alternate sides so the book keeps one resting order per level and
        // every admitted frame crosses or rests deterministically.
        let id = u64::try_from(WARMUP + offset + 1).unwrap_or(u64::MAX);
        let bytes = workload_frame(id);
        let elapsed_ns = if ADMIT {
            admit_step(&mut gateway, &mut session, &mut reports, id, &bytes)
        } else {
            // Re-offer the previous sequence: always a duplicate refusal.
            reject_step(&mut session, initial_sequence.0 - 1)
        };
        *sample = u64::try_from(elapsed_ns).unwrap_or(u64::MAX);
        checksum = checksum.rotate_left(7) ^ id ^ session.expected_sequence().0;
        for report in reports.iter() {
            checksum ^= report.maker_order_id.0.rotate_left(13)
                ^ report.taker_order_id.0.rotate_left(23)
                ^ report.quantity.0;
        }
    }
    let allocations = ALLOCATIONS.load(Ordering::SeqCst) - allocations_before;
    let deallocations = DEALLOCATIONS.load(Ordering::SeqCst) - deallocations_before;
    if ADMIT {
        assert_eq!(
            session.expected_sequence().0,
            initial_sequence.0 + samples as u64
        );
        let expected_accounts = match samples % 4 {
            0 => [(0, 0), (0, 0)],
            1 => [(-5, 1), (0, 0)],
            2 => [(-5, 0), (5, 0)],
            _ => [(-5, 0), (0, 1)],
        };
        for (account, expected) in [AccountId(1), AccountId(2)]
            .into_iter()
            .zip(expected_accounts)
        {
            assert_eq!(gateway.risk().account_snapshot(account), Some(expected));
        }
        assert!(gateway.top_level(Side::Buy).is_none());
        assert_eq!(gateway.top_level(Side::Sell).is_some(), samples % 2 == 1);
    } else {
        assert_eq!(gateway.stable_digest(), initial_digest);
        assert_eq!(gateway.expected_sequence(), initial_sequence);
        assert_eq!(session.deadline(), initial_deadline);
    }
    assert_eq!(allocations, 0, "{scenario} allocations");
    assert_eq!(deallocations, 0, "{scenario} deallocations");

    let mut record = BenchRecord::new(
        "component",
        "session",
        scenario,
        &[("tif", Extra::Text("session"))],
    );
    let stats = analyze(&mut latencies);
    record.samples = samples;
    record.allocations = allocations;
    record.deallocations = deallocations;
    record.checksum =
        checksum ^ gateway.stable_digest() ^ session.expected_sequence().0.rotate_left(31);
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

fn workload_frame(id: u64) -> [u8; 46] {
    // Reverse account roles after each pair so net positions remain bounded.
    let account = match (id - 1) % 4 {
        0 | 3 => 1,
        _ => 2,
    };
    frame(
        id,
        account,
        if id % 2 == 0 { Side::Buy } else { Side::Sell },
    )
}

pub fn session_benchmark(samples: usize, out: &mut Vec<BenchRecord>) {
    run_cell::<true>(samples, "session_active_admission", out);
    run_cell::<false>(samples, "session_active_rejection", out);
}

/// Confirms half the window untimed. Each timed sample refuses a session
/// sequence gap, retains 32 frames, refuses full-window retention, and replays
/// all 64 retained payloads. This is an in-memory window, not journal recovery.
///
/// # Panics
///
/// Panics on fixture failure or an allocation gate trip.
pub fn recovery_benchmark(samples: usize, out: &mut Vec<BenchRecord>) {
    const CAPACITY: usize = 64;
    use hft_session::retransmit::{RetainError, RetransmitBuffer};
    use hft_types::SequenceNumber;

    if samples == 0 {
        return;
    }
    let mut buffer = RetransmitBuffer::new(CAPACITY);
    let mut latencies = vec![0_u64; samples];
    let mut checksum = 0_u64;
    let payload = [0_u8; 46];
    let (_, mut session, _) = active_pair();

    // Warm-up: fill the window once, untimed.
    for seq in 1..=CAPACITY as u64 {
        let mut stored = payload;
        stored[..8].copy_from_slice(&seq.to_be_bytes());
        buffer
            .retain(SequenceNumber(seq), &stored)
            .expect("warm-up retention");
    }

    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    for sample in &mut latencies {
        // Advance confirmation with the window, including after wraparound.
        let confirmed = buffer.next_sequence() - CAPACITY as u64 + 31;
        assert_eq!(buffer.confirm_through(confirmed), 32);

        let started = Instant::now();
        let gap = session.handle(
            SessionEvent::Command {
                sequence: SequenceNumber(2),
            },
            0,
        );
        let mut seq = buffer.next_sequence();
        while buffer.remaining_capacity() > 0 {
            let mut stored = payload;
            stored[..8].copy_from_slice(&seq.to_be_bytes());
            buffer
                .retain(SequenceNumber(seq), &stored)
                .expect("refill retention");
            seq += 1;
        }
        let full = buffer.retain(SequenceNumber(seq), &payload);
        let mut replayed = 0;
        for frame in buffer.since(confirmed + 1) {
            checksum = checksum.rotate_left(7) ^ frame.sequence.0;
            for byte in std::hint::black_box(frame.slice()) {
                checksum = checksum.rotate_left(3) ^ u64::from(*byte);
            }
            replayed += 1;
        }
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        assert_eq!(
            gap,
            Err(SessionError::Gap {
                expected: SequenceNumber(1),
                received: SequenceNumber(2),
            })
        );
        assert_eq!(session.expected_sequence(), SequenceNumber(1));
        assert_eq!(full, Err(RetainError::Full));
        assert_eq!(buffer.next_sequence(), seq);
        assert_eq!(replayed, CAPACITY);
        for (offset, frame) in buffer.since(confirmed + 1).enumerate() {
            let expected = confirmed + 1 + offset as u64;
            assert_eq!(frame.sequence, SequenceNumber(expected));
            assert_eq!(&frame.slice()[..8], &expected.to_be_bytes());
            assert_eq!(frame.len, payload.len());
        }
    }

    let allocations = ALLOCATIONS.load(Ordering::SeqCst) - allocations_before;
    let deallocations = DEALLOCATIONS.load(Ordering::SeqCst) - deallocations_before;
    assert_eq!(allocations, 0, "recovery allocations");
    assert_eq!(deallocations, 0, "recovery deallocations");
    let sample_count = latencies.len();
    let stats = analyze(&mut latencies);
    let mut record = BenchRecord::new(
        "component",
        "session",
        "recovery_window",
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
    record.checksum = checksum;
    record.allocations = allocations;
    record.deallocations = deallocations;
    out.push(record);
}
