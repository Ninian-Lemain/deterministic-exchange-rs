#![deny(unsafe_op_in_unsafe_fn)]

use hft_gateway::Gateway;
use hft_io::RxFrame;
use hft_risk::{RiskEngine, RiskLimits};
use hft_spsc::SpscQueue;
use hft_types::{
    AccountId, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, ReportBuffer, SequenceNumber,
    Side,
};
use hft_wire::encode_new_order;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);

// SAFETY: every operation delegates to `System` with the identical pointer and
// layout contract. Counters do not affect allocation ownership or alignment.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller provides GlobalAlloc's valid layout contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller returns the pointer with its original layout.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller provides the allocation and new-size contracts.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn main() {
    const ITERATIONS: u64 = 100_000;
    const LATENCY_SAMPLES: usize = 2_000;
    let limits = RiskLimits {
        max_quantity: Quantity(10),
        max_notional: 10_000,
        max_abs_position: Quantity(ITERATIONS + 1),
        max_open_orders: 8,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    };
    let mut risk = RiskEngine::<2, 8>::new();
    risk.register_account(AccountId(1), limits)
        .expect("benchmark account one");
    risk.register_account(AccountId(2), limits)
        .expect("benchmark account two");
    let mut gateway = Gateway::<2, 8, 2, 2>::new(risk, InstrumentId(1));
    let mut reports = ReportBuffer::<1>::new();

    run_pair(&mut gateway, &mut reports, 1).expect("warm-up path");
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    let started = Instant::now();
    let mut samples = [0_u64; LATENCY_SAMPLES];
    for iteration in 0..ITERATIONS {
        if iteration < (LATENCY_SAMPLES / 2) as u64 {
            let first_id = iteration * 2 + 3;
            let sell = encode_new_order(order(first_id, AccountId(1), Side::Sell));
            let sample_started = Instant::now();
            gateway
                .process_frame(&RxFrame::from_bytes(&sell), &mut reports)
                .expect("sampled sell");
            let index = usize::try_from(iteration).expect("sample index");
            samples[index * 2] =
                u64::try_from(sample_started.elapsed().as_nanos()).unwrap_or(u64::MAX);

            let buy = encode_new_order(order(first_id + 1, AccountId(2), Side::Buy));
            let sample_started = Instant::now();
            gateway
                .process_frame(&RxFrame::from_bytes(&buy), &mut reports)
                .expect("sampled buy");
            samples[index * 2 + 1] =
                u64::try_from(sample_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        } else {
            run_pair(&mut gateway, &mut reports, iteration * 2 + 3).expect("measured path");
        }
    }
    let elapsed = started.elapsed();
    let allocations_after = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(allocations_after, allocations_before, "hot-path allocation");
    assert_eq!(
        deallocations_after, deallocations_before,
        "hot-path deallocation"
    );
    let messages = ITERATIONS * 2;
    samples.sort_unstable();
    let (maximum_occupancy, backpressure_events) = queue_capacity_smoke();
    println!(
        "messages={messages} elapsed_ns={} mean_ns={} p50_ns={} p90_ns={} p99_ns={} p99_9_ns={} max_ns={} queue_max_occupancy={} backpressure_events={} allocations=0 deallocations=0 digest={:016x}",
        elapsed.as_nanos(),
        elapsed.as_nanos() / u128::from(messages),
        percentile(&samples, 500),
        percentile(&samples, 900),
        percentile(&samples, 990),
        percentile(&samples, 999),
        samples[LATENCY_SAMPLES - 1],
        maximum_occupancy,
        backpressure_events,
        gateway.stable_digest()
    );
}

fn queue_capacity_smoke() -> (usize, usize) {
    let mut queue = SpscQueue::<u64, 64>::try_new().expect("valid queue capacity");
    let (mut producer, mut consumer) = queue.split();
    let mut occupancy = 0_usize;
    let mut maximum = 0_usize;
    let mut backpressure = 0_usize;
    for value in 0..65 {
        if producer.try_push(value).is_ok() {
            occupancy += 1;
            maximum = maximum.max(occupancy);
        } else {
            backpressure += 1;
        }
    }
    while consumer.try_pop().is_some() {
        occupancy -= 1;
    }
    debug_assert_eq!(occupancy, 0);
    (maximum, backpressure)
}

fn percentile(samples: &[u64], permille: usize) -> u64 {
    let index = samples
        .len()
        .saturating_mul(permille)
        .div_ceil(1_000)
        .saturating_sub(1)
        .min(samples.len().saturating_sub(1));
    samples[index]
}

fn run_pair(
    gateway: &mut Gateway<2, 8, 2, 2>,
    reports: &mut ReportBuffer<1>,
    first_id: u64,
) -> Result<(), hft_gateway::GatewayError> {
    let sell = encode_new_order(order(first_id, AccountId(1), Side::Sell));
    gateway.process_frame(&RxFrame::from_bytes(&sell), reports)?;
    let buy = encode_new_order(order(first_id + 1, AccountId(2), Side::Buy));
    gateway.process_frame(&RxFrame::from_bytes(&buy), reports)?;
    Ok(())
}

const fn order(id: u64, account_id: AccountId, side: Side) -> NewOrder {
    NewOrder {
        order_id: OrderId(id),
        account_id,
        instrument_id: InstrumentId(1),
        price: PriceTicks(100),
        quantity: Quantity(1),
        sequence: SequenceNumber(id),
        side,
    }
}
