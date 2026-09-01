use crate::record::{BenchRecord, Extra};
use crate::{ALLOCATIONS, DEALLOCATIONS, push_latency_record};
use hft_events::{BoundedEventEngine, EventBatch, EventEngineError};
use hft_gateway::Gateway;
use hft_io::RxFrame;
use hft_risk::{RiskEngine, RiskLimits};
use hft_spsc::SpscQueue;
use hft_types::{
    AccountId, CancelOrder, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, SequenceNumber,
    Side, TimeInForce,
};
use hft_wire::{encode_cancel_order, encode_new_order};
use std::sync::atomic::Ordering;
use std::time::Instant;

const EVENT_BATCH_CAPACITY: usize = 3;
const EVENT_QUEUE_CAPACITY: usize = 64;
const WARMUP: usize = 128;

type BenchBatch = EventBatch<EVENT_BATCH_CAPACITY>;

pub fn event_benchmarks(samples: usize, out: &mut Vec<BenchRecord>) {
    admitted_push_pop(samples, out);
    full_queue_backpressure(samples, out);
}

fn gateway() -> Gateway<2, 16, 4, 4> {
    let limits = RiskLimits {
        max_quantity: Quantity(10),
        max_notional: 10_000,
        max_abs_position: Quantity(1_000),
        max_open_orders: 16,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    };
    let mut risk = RiskEngine::new();
    risk.register_account(AccountId(1), limits)
        .expect("event benchmark account");
    risk.register_account(AccountId(2), limits)
        .expect("event benchmark peer account");
    Gateway::new(risk, InstrumentId(1))
}

fn order(order_id: u64, sequence: u64) -> [u8; 46] {
    encode_new_order(NewOrder {
        order_id: OrderId(order_id),
        account_id: AccountId(1),
        instrument_id: InstrumentId(1),
        price: PriceTicks(100),
        quantity: Quantity(1),
        sequence: SequenceNumber(sequence),
        side: Side::Buy,
        time_in_force: TimeInForce::Gtc,
    })
}

fn cancel(order_id: u64, sequence: u64) -> [u8; 28] {
    encode_cancel_order(CancelOrder {
        order_id: OrderId(order_id),
        account_id: AccountId(1),
        instrument_id: InstrumentId(1),
        sequence: SequenceNumber(sequence),
    })
}

fn admitted_push_pop(samples: usize, out: &mut Vec<BenchRecord>) {
    let mut queue =
        SpscQueue::<BenchBatch, EVENT_QUEUE_CAPACITY>::try_new().expect("event benchmark queue");
    let (producer, mut consumer) = queue.split();
    let mut engine =
        BoundedEventEngine::<2, 16, 4, 4, 1, EVENT_BATCH_CAPACITY, EVENT_QUEUE_CAPACITY>::try_new(
            gateway(),
            producer,
        )
        .expect("event benchmark batch");
    let sample_count = samples.min(2_000);
    let mut timings = [0_u64; 2_000];
    let mut event_count = 0_u64;

    for iteration in 0..WARMUP {
        let order_id = u64::try_from(iteration)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let sequence = order_id.saturating_mul(2).saturating_sub(1);
        let frame = order(order_id, sequence);
        engine
            .process_frame(&RxFrame::from_bytes(&frame))
            .expect("admitted warmup command");
        let _ = consumer.try_pop().expect("warmup event batch");
        let cleanup = cancel(order_id, sequence.saturating_add(1));
        engine
            .process_frame(&RxFrame::from_bytes(&cleanup))
            .expect("cleanup command");
        let _ = consumer.try_pop().expect("cleanup event batch");
    }

    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    for (iteration, sample) in timings[..sample_count].iter_mut().enumerate() {
        let order_id = u64::try_from(WARMUP.saturating_add(iteration))
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let sequence = order_id.saturating_mul(2).saturating_sub(1);
        let frame = order(order_id, sequence);
        let started = Instant::now();
        engine
            .process_frame(&RxFrame::from_bytes(&frame))
            .expect("admitted measured command");
        let batch = consumer.try_pop().expect("measured event batch");
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        event_count = event_count.saturating_add(u64::try_from(batch.len()).unwrap_or(u64::MAX));

        let cleanup = cancel(order_id, sequence.saturating_add(1));
        engine
            .process_frame(&RxFrame::from_bytes(&cleanup))
            .expect("cleanup command");
        let _ = consumer.try_pop().expect("cleanup event batch");
    }

    let allocations = ALLOCATIONS
        .load(Ordering::SeqCst)
        .saturating_sub(allocations_before);
    let deallocations = DEALLOCATIONS
        .load(Ordering::SeqCst)
        .saturating_sub(deallocations_before);
    push_latency_record(
        out,
        BenchRecord {
            allocations,
            deallocations,
            checksum: engine.gateway().stable_digest() ^ event_count,
            ..BenchRecord::new(
                "gateway",
                "events",
                "admitted_push_pop",
                &[
                    (
                        "event_batch_capacity",
                        Extra::U64(u64::try_from(EVENT_BATCH_CAPACITY).unwrap_or(u64::MAX)),
                    ),
                    (
                        "queue_capacity",
                        Extra::U64(u64::try_from(EVENT_QUEUE_CAPACITY).unwrap_or(u64::MAX)),
                    ),
                    ("queue_occupancy", Extra::U64(1)),
                    ("event_count", Extra::U64(event_count)),
                    ("backpressure_count", Extra::U64(0)),
                ],
            )
        },
        &mut timings[..sample_count],
    );
}

fn full_queue_backpressure(samples: usize, out: &mut Vec<BenchRecord>) {
    const QUEUE_CAPACITY: usize = 1;
    let mut queue = SpscQueue::<BenchBatch, QUEUE_CAPACITY>::try_new().expect("full event queue");
    let (producer, _consumer) = queue.split();
    let mut engine =
        BoundedEventEngine::<2, 16, 4, 4, 1, EVENT_BATCH_CAPACITY, QUEUE_CAPACITY>::try_new(
            gateway(),
            producer,
        )
        .expect("event benchmark batch");
    let first = order(1, 1);
    engine
        .process_frame(&RxFrame::from_bytes(&first))
        .expect("fill event queue");
    let refused = cancel(1, 2);
    let sample_count = samples.min(2_000);
    let mut timings = [0_u64; 2_000];
    for _ in 0..WARMUP {
        assert_eq!(
            engine.process_frame(&RxFrame::from_bytes(&refused)),
            Err(EventEngineError::Backpressured)
        );
    }
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    let mut backpressure_count = 0_u64;
    for sample in &mut timings[..sample_count] {
        let started = Instant::now();
        let result = engine.process_frame(&RxFrame::from_bytes(&refused));
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if result == Err(EventEngineError::Backpressured) {
            backpressure_count += 1;
        }
    }
    let allocations = ALLOCATIONS
        .load(Ordering::SeqCst)
        .saturating_sub(allocations_before);
    let deallocations = DEALLOCATIONS
        .load(Ordering::SeqCst)
        .saturating_sub(deallocations_before);
    push_latency_record(
        out,
        BenchRecord {
            allocations,
            deallocations,
            checksum: engine.gateway().stable_digest() ^ backpressure_count,
            ..BenchRecord::new(
                "gateway",
                "events",
                "full_queue_backpressure",
                &[
                    (
                        "event_batch_capacity",
                        Extra::U64(u64::try_from(EVENT_BATCH_CAPACITY).unwrap_or(u64::MAX)),
                    ),
                    (
                        "queue_capacity",
                        Extra::U64(u64::try_from(QUEUE_CAPACITY).unwrap_or(u64::MAX)),
                    ),
                    ("queue_occupancy", Extra::U64(1)),
                    ("event_count", Extra::U64(0)),
                    ("backpressure_count", Extra::U64(backpressure_count)),
                ],
            )
        },
        &mut timings[..sample_count],
    );
}
