//! Extra benchmark workloads emitting structured records for the reporter:
//! wire-format parsing, SPSC ring traffic, a seeded gateway command mix, and
//! deep-book taker sweeps.

use crate::record::{BenchRecord, Extra};
use crate::{ALLOCATIONS, DEALLOCATIONS, analyze, time_samples};
use hft_book::OrderBook;
use hft_gateway::Gateway;
use hft_io::RxFrame;
use hft_model::{Command, CommandGen, GenConfig, Rng};
use hft_risk::{RiskEngine, RiskLimits};
use hft_spsc::{Consumer, Producer, SpscQueue};
use hft_types::{
    AccountId, CancelOrder, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, ReportBuffer,
    SequenceNumber, Side,
};
use hft_wire::{
    BorrowedMessage, CANCEL_ORDER_LEN, NEW_ORDER_LEN, encode_cancel_order, encode_new_order,
    parse_message,
};
use std::sync::atomic::Ordering;
use std::time::Instant;

/// Alternating new-order and cancel-frame parsing through the wire decoder.
///
/// Frame construction stays outside the timed region; every parsed sequence
/// number feeds the checksum.
///
/// # Panics
///
/// Panics if a pre-encoded frame fails to parse or an allocation gate trips.
pub fn parser_benchmark(samples: usize, out: &mut Vec<BenchRecord>) {
    const WARMUP_STEPS: usize = 64;
    if samples == 0 {
        return;
    }
    let new_frames = samples.div_ceil(2);
    let cancel_frames = samples / 2;
    let mut frames_new: Vec<[u8; NEW_ORDER_LEN]> = Vec::with_capacity(new_frames);
    let mut frames_cancel: Vec<[u8; CANCEL_ORDER_LEN]> = Vec::with_capacity(cancel_frames);
    for i in 0..new_frames {
        let index = u64::try_from(i).expect("frame index fits u64");
        frames_new.push(encode_new_order(NewOrder {
            time_in_force: hft_types::TimeInForce::Gtc,
            order_id: OrderId(index + 1),
            account_id: AccountId(u32::try_from(1 + (i % 2)).expect("account fits u32")),
            instrument_id: InstrumentId(1),
            price: PriceTicks(i64::try_from(100 + (i % 50)).expect("price fits i64")),
            quantity: Quantity(u64::try_from(1 + (i % 4)).expect("quantity fits u64")),
            sequence: SequenceNumber(index + 1),
            side: if i % 2 == 0 { Side::Buy } else { Side::Sell },
        }));
    }
    let sample_total = u64::try_from(samples).expect("sample count fits u64");
    for i in 0..cancel_frames {
        let index = u64::try_from(i).expect("frame index fits u64");
        frames_cancel.push(encode_cancel_order(CancelOrder {
            order_id: OrderId(index + 1),
            account_id: AccountId(u32::try_from(1 + (i % 2)).expect("account fits u32")),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(sample_total + index + 1),
        }));
    }

    let mut checksum = 0_u64;
    for index in 0..WARMUP_STEPS.min(samples) {
        checksum ^= parse_one(&frames_new, &frames_cancel, index);
    }
    let mut latencies = vec![0_u64; samples];
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    time_samples(&mut latencies, |index| {
        let index = usize::try_from(index).expect("sample index fits usize");
        checksum ^= parse_one(&frames_new, &frames_cancel, index);
    });
    let allocations_after = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(
        allocations_after, allocations_before,
        "parser benchmark allocations"
    );
    assert_eq!(
        deallocations_after, deallocations_before,
        "parser benchmark deallocations"
    );

    finish_record(
        out,
        BenchRecord {
            allocations: allocations_after - allocations_before,
            deallocations: deallocations_after - deallocations_before,
            checksum,
            ..BenchRecord::new(
                "network",
                "parser",
                "parse_frames",
                &[
                    (
                        "new_frames",
                        Extra::U64(u64::try_from(new_frames).unwrap_or(u64::MAX)),
                    ),
                    (
                        "cancel_frames",
                        Extra::U64(u64::try_from(cancel_frames).unwrap_or(u64::MAX)),
                    ),
                ],
            )
        },
        &mut latencies,
    );
}

/// Decodes the alternating frame at `index` and returns its sequence number.
fn parse_one(
    frames_new: &[[u8; NEW_ORDER_LEN]],
    frames_cancel: &[[u8; CANCEL_ORDER_LEN]],
    index: usize,
) -> u64 {
    let frame = if index % 2 == 0 {
        &frames_new[index >> 1][..]
    } else {
        &frames_cancel[index >> 1][..]
    };
    let frame_view = RxFrame::from_bytes(frame);
    let parsed = parse_message(&frame_view).expect("encoded frames stay valid");
    owned_sequence(parsed)
}

/// Sequence number of either decoded message kind.
fn owned_sequence(message: BorrowedMessage<'_>) -> u64 {
    match message {
        BorrowedMessage::NewOrder(order) => order.to_owned().sequence.0,
        BorrowedMessage::CancelOrder(cancel) => cancel.to_owned().sequence.0,
    }
}

/// Occupancy and checksum tracking for one SPSC walk.
struct WalkState {
    occupancy: usize,
    high_water: usize,
    backpressure_events: u64,
    checksum: u64,
    step: u64,
}

fn walk_value(step: u64) -> u64 {
    step ^ 0xA5A5_A5A5_A5A5_A5A5
}

fn push_tracked<const N: usize>(
    producer: &mut Producer<'_, u64, N>,
    value: u64,
    state: &mut WalkState,
) {
    match producer.try_push(value) {
        Ok(()) => {
            state.occupancy += 1;
            state.high_water = state.high_water.max(state.occupancy);
        }
        Err(rejected) => {
            state.backpressure_events += 1;
            state.checksum ^= rejected;
        }
    }
}

fn pop_tracked<const N: usize>(consumer: &mut Consumer<'_, u64, N>, state: &mut WalkState) {
    if let Some(popped) = consumer.try_pop() {
        state.checksum ^= popped;
        state.occupancy = state.occupancy.saturating_sub(1);
    }
}

fn walk_step<const N: usize>(
    producer: &mut Producer<'_, u64, N>,
    consumer: &mut Consumer<'_, u64, N>,
    action: u64,
    value: u64,
    state: &mut WalkState,
) {
    match action {
        0 => push_tracked(producer, value, state),
        1 => pop_tracked(consumer, state),
        _ => {
            push_tracked(producer, value, state);
            pop_tracked(consumer, state);
        }
    }
}

/// Seeded random walk of pushes and pops against one fixed-capacity SPSC
/// ring. Random draws stay outside the timed window so only queue operations
/// are measured; the walk ends with an untimed drain folded into the
/// checksum.
///
/// # Panics
///
/// Panics if the queue capacity is invalid or an allocation gate trips.
pub fn spsc_benchmark(samples: usize, seed: u64, out: &mut Vec<BenchRecord>) {
    const CAPACITY: usize = 1024;
    const WARMUP_STEPS: u64 = 64;
    if samples == 0 {
        return;
    }
    let mut queue = SpscQueue::<u64, CAPACITY>::try_new().expect("capacity is a power of two");
    let (mut producer, mut consumer) = queue.split();
    let mut rng = Rng::new(seed);
    let mut state = WalkState {
        occupancy: 0,
        high_water: 0,
        backpressure_events: 0,
        checksum: 0,
        step: 0,
    };

    for _ in 0..WARMUP_STEPS {
        let action = rng.below(3);
        let value = walk_value(state.step);
        state.step += 1;
        walk_step(&mut producer, &mut consumer, action, value, &mut state);
    }
    let mut latencies = vec![0_u64; samples];
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    for sample in &mut latencies {
        let action = rng.below(3);
        let value = walk_value(state.step);
        state.step += 1;
        let started = Instant::now();
        walk_step(&mut producer, &mut consumer, action, value, &mut state);
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    }
    while let Some(popped) = consumer.try_pop() {
        state.checksum ^= popped;
        state.occupancy = state.occupancy.saturating_sub(1);
    }
    let allocations_after = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(
        allocations_after, allocations_before,
        "spsc benchmark allocations"
    );
    assert_eq!(
        deallocations_after, deallocations_before,
        "spsc benchmark deallocations"
    );

    finish_record(
        out,
        BenchRecord {
            allocations: allocations_after - allocations_before,
            deallocations: deallocations_after - deallocations_before,
            checksum: state.checksum,
            ..BenchRecord::new(
                "component",
                "spsc",
                "push_pop_walk",
                &[
                    (
                        "capacity",
                        Extra::U64(u64::try_from(CAPACITY).unwrap_or(u64::MAX)),
                    ),
                    (
                        "max_occupancy",
                        Extra::U64(u64::try_from(state.high_water).unwrap_or(u64::MAX)),
                    ),
                    ("backpressure_events", Extra::U64(state.backpressure_events)),
                ],
            )
        },
        &mut latencies,
    );
}

/// Encodes one generated command into the shared frame buffer and returns its
/// wire length.
fn encode_command(command: Command, frame: &mut [u8; NEW_ORDER_LEN]) -> usize {
    match command {
        Command::New(order) => {
            *frame = encode_new_order(order);
            NEW_ORDER_LEN
        }
        Command::Cancel(cancel) => {
            frame[..CANCEL_ORDER_LEN].copy_from_slice(&encode_cancel_order(cancel));
            CANCEL_ORDER_LEN
        }
    }
}

/// Seeded mixed new-order and cancel traffic through the full gateway path.
/// Generation and encoding stay outside the timed window; rejections are part
/// of the workload and leave the recorded latency untouched.
///
/// # Panics
///
/// Panics on account-registration failure or an allocation gate trip.
pub fn gateway_mixed_benchmark(commands: u64, warmup: u64, seed: u64, out: &mut Vec<BenchRecord>) {
    let limits = RiskLimits {
        max_quantity: Quantity(4),
        max_notional: 1_000_000,
        max_abs_position: Quantity(1_000_000),
        max_open_orders: 32,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(10_000),
    };
    let mut risk = RiskEngine::<2, 64>::new();
    risk.register_account(AccountId(1), limits)
        .expect("account one registers");
    risk.register_account(AccountId(2), limits)
        .expect("account two registers");
    let mut gateway = Gateway::<2, 64, 16, 16>::new(risk, InstrumentId(1));
    let mut reports = ReportBuffer::<16>::new();
    let mut generator = CommandGen::new(
        GenConfig {
            accounts: 2,
            minimum_price: 98,
            maximum_price: 102,
            max_quantity: 4,
            cancel_probability_pct: 40,
            duplicate_id_probability_pct: 3,
            ioc_probability_pct: 15,
            fok_probability_pct: 10,
        },
        InstrumentId(1),
        seed,
    );
    let measured = commands.saturating_sub(warmup);
    if measured == 0 {
        return;
    }
    let mut latencies: Vec<u64> =
        Vec::with_capacity(usize::try_from(measured).expect("measured fits usize"));
    let mut frame = [0_u8; NEW_ORDER_LEN];

    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    for i in 0..commands {
        let command = generator.next_command();
        let length = encode_command(command, &mut frame);
        if i < warmup {
            // Warm-up commands must be processed too; skipping them would
            // break session sequencing for every later frame.
            let _ = gateway.process_frame(&RxFrame::from_bytes(&frame[..length]), &mut reports);
            continue;
        }
        let started = Instant::now();
        let _ = gateway.process_frame(&RxFrame::from_bytes(&frame[..length]), &mut reports);
        latencies.push(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
    }
    let checksum = gateway.stable_digest();
    let allocations_after = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(
        allocations_after, allocations_before,
        "gateway benchmark allocations"
    );
    assert_eq!(
        deallocations_after, deallocations_before,
        "gateway benchmark deallocations"
    );

    finish_record(
        out,
        BenchRecord {
            allocations: allocations_after - allocations_before,
            deallocations: deallocations_after - deallocations_before,
            checksum,
            ..BenchRecord::new(
                "gateway",
                "gateway",
                "mixed_seeded",
                &[("commands", Extra::U64(commands))],
            )
        },
        &mut latencies,
    );
}

/// Taker traversal depths cycled per sample.
const TRAVERSAL_DEPTHS: [usize; 3] = [1, 8, 64];

/// One deep-book taker sweep; returns elapsed nanoseconds around the submit.
/// Order construction and the report reset stay outside the timed window.
fn deep_taker_step(
    book: &mut OrderBook<128, 8>,
    reports: &mut ReportBuffer<64>,
    next_maker_id: &mut u64,
    depth_index: usize,
    checksum: &mut u64,
) -> u64 {
    let quantity = u64::try_from(TRAVERSAL_DEPTHS[depth_index % 3]).expect("depth fits u64");
    let taker_id = 1_000_000 + *next_maker_id;
    *next_maker_id += 1;
    let order = NewOrder {
        time_in_force: hft_types::TimeInForce::Gtc,
        order_id: OrderId(taker_id),
        account_id: AccountId(2),
        instrument_id: InstrumentId(1),
        price: PriceTicks(999_999),
        quantity: Quantity(quantity),
        sequence: SequenceNumber(taker_id),
        side: Side::Buy,
    };
    let started = Instant::now();
    let summary = book.submit(order, reports).expect("deep taker fills");
    let elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    assert_eq!(
        summary.filled_quantity.0, quantity,
        "taker fills completely"
    );
    assert_eq!(
        u64::try_from(summary.report_count).unwrap_or(u64::MAX),
        quantity,
        "one fill per traversed level"
    );
    *checksum ^= summary.filled_quantity.0;
    // Replenish one maker at each consumed price so the book shape stays
    // constant across samples. Prices are staged on the stack because the
    // reports buffer is both the iteration source and the submit target.
    let mut consumed = [(0_i64, 1_u64); 64];
    let consumed_count = reports.len();
    for (slot, report) in consumed.iter_mut().zip(reports.iter()) {
        *slot = (report.price.0, report.quantity.0);
    }
    reports.clear();
    for &(price, _) in &consumed[..consumed_count] {
        let id = *next_maker_id;
        *next_maker_id += 1;
        book.submit(
            NewOrder {
                time_in_force: hft_types::TimeInForce::Gtc,
                order_id: OrderId(id),
                account_id: AccountId(1),
                instrument_id: InstrumentId(1),
                price: PriceTicks(price),
                quantity: Quantity(1),
                sequence: SequenceNumber(id),
                side: Side::Sell,
            },
            reports,
        )
        .expect("replenish consumed level");
        reports.clear();
    }
    elapsed_ns
}

/// Taker sweeps against a 64-level deep ask book with one single-unit maker
/// per level; each sample consumes exactly `k` levels and the untimed
/// teardown restores them. Fixture construction and warm-up stay outside the
/// timed region.
///
/// # Panics
///
/// Panics if a taker does not fill completely, a replenish fails, or an
/// allocation gate trips.
pub fn deep_book_benchmark(samples: usize, seed: u64, out: &mut Vec<BenchRecord>) {
    const WARMUP_SAMPLES: usize = 32;
    if samples == 0 {
        return;
    }
    // Heap placement keeps the ~300 KB book off the 1 MB Windows stack.
    let mut book = std::boxed::Box::new(OrderBook::<128, 8>::new(InstrumentId(1)));
    let mut reports = ReportBuffer::<64>::new();
    for level_index in 0..64_usize {
        let id = u64::try_from(level_index + 1).expect("maker id fits u64");
        let price = 100 + i64::try_from(level_index).expect("level index fits i64");
        book.submit(
            NewOrder {
                time_in_force: hft_types::TimeInForce::Gtc,
                order_id: OrderId(id),
                account_id: AccountId(1),
                instrument_id: InstrumentId(1),
                price: PriceTicks(price),
                quantity: Quantity(1),
                sequence: SequenceNumber(id),
                side: Side::Sell,
            },
            &mut reports,
        )
        .expect("fixture maker rests");
        reports.clear();
    }
    // Depths cycle deterministically today; the seed keeps future seeded
    // distributions reproducible.
    let _ = seed;
    let mut checksum = 0_u64;
    let mut next_maker_id = 65_u64;
    for depth_index in 0..WARMUP_SAMPLES {
        deep_taker_step(
            &mut book,
            &mut reports,
            &mut next_maker_id,
            depth_index,
            &mut checksum,
        );
    }
    let mut latencies = vec![0_u64; samples];
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    for (offset, sample) in latencies.iter_mut().enumerate() {
        *sample = deep_taker_step(
            &mut book,
            &mut reports,
            &mut next_maker_id,
            WARMUP_SAMPLES + offset,
            &mut checksum,
        );
    }
    let allocations_after = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(
        allocations_after, allocations_before,
        "deep book benchmark allocations"
    );
    assert_eq!(
        deallocations_after, deallocations_before,
        "deep book benchmark deallocations"
    );

    finish_record(
        out,
        BenchRecord {
            allocations: allocations_after - allocations_before,
            deallocations: deallocations_after - deallocations_before,
            checksum,
            ..BenchRecord::new(
                "component",
                "book",
                "deep_book",
                &[
                    ("levels", Extra::U64(64)),
                    ("orders_per_level", Extra::U64(1)),
                ],
            )
        },
        &mut latencies,
    );
}

/// Computes latency statistics over the sample slice, completes the pending
/// record fields, and appends the record.
fn finish_record(out: &mut Vec<BenchRecord>, mut record: BenchRecord, latencies: &mut [u64]) {
    let stats = analyze(latencies);
    let mean_ns = u64::try_from(stats.mean).unwrap_or(u64::MAX);
    record.samples = latencies.len();
    record.mean_ns = mean_ns;
    record.p50_ns = stats.p50;
    record.p90_ns = stats.p90;
    record.p99_ns = stats.p99;
    record.p99_9_ns = stats.p99_9;
    record.max_ns = stats.max;
    record.ops_per_second = 1_000_000_000_u64.checked_div(mean_ns).unwrap_or(0);
    out.push(record);
}
