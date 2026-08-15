#![deny(unsafe_op_in_unsafe_fn)]

use hft_book::OrderBook;
use hft_gateway::Gateway;
use hft_io::RxFrame;
use hft_risk::{RiskEngine, RiskLimits};
use hft_spsc::SpscQueue;
use hft_types::{
    AccountId, CancelOrder, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, ReportBuffer,
    SequenceNumber, Side,
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
    cancel_benchmark();
    fifo_benchmark();
}

#[derive(Clone, Copy)]
enum FifoScenario {
    HeadCancel,
    MiddleCancel,
    TailCancel,
    HeadFill,
}

impl FifoScenario {
    const ALL: [Self; 4] = [
        Self::HeadCancel,
        Self::MiddleCancel,
        Self::TailCancel,
        Self::HeadFill,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::HeadCancel => "head_cancel",
            Self::MiddleCancel => "middle_cancel",
            Self::TailCancel => "tail_cancel",
            Self::HeadFill => "head_fill",
        }
    }
}

fn fifo_benchmark() {
    for scenario in FifoScenario::ALL {
        run_fifo_cell::<1>(scenario);
        run_fifo_cell::<4>(scenario);
        run_fifo_cell::<16>(scenario);
        run_fifo_cell::<64>(scenario);
        run_fifo_cell::<512>(scenario);
    }
}

fn run_fifo_cell<const DEPTH: usize>(scenario: FifoScenario) {
    const SAMPLES: usize = 2_000;
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    let mut samples_ns = [0_u64; SAMPLES];
    let mut checksum = 0_u64;
    let target = match scenario {
        FifoScenario::HeadCancel | FifoScenario::HeadFill => 1,
        FifoScenario::MiddleCancel => DEPTH.div_ceil(2),
        FifoScenario::TailCancel => DEPTH,
    };
    let target = u64::try_from(target).expect("target id fits u64");
    for sample in &mut samples_ns {
        let mut book = fifo_fixture::<DEPTH>();
        let mut reports = ReportBuffer::<1>::new();
        let started = Instant::now();
        match scenario {
            FifoScenario::HeadCancel | FifoScenario::MiddleCancel | FifoScenario::TailCancel => {
                let cancelled = book
                    .cancel(CancelOrder {
                        order_id: OrderId(target),
                        account_id: AccountId(1),
                        instrument_id: InstrumentId(1),
                        sequence: SequenceNumber(target),
                    })
                    .expect("fifo cancel");
                checksum ^= cancelled.order_id.0;
            }
            FifoScenario::HeadFill => {
                let taker = u64::try_from(DEPTH).expect("depth fits u64") + 1;
                let summary = book
                    .submit(
                        NewOrder {
                            order_id: OrderId(taker),
                            account_id: AccountId(2),
                            instrument_id: InstrumentId(1),
                            price: PriceTicks(100),
                            quantity: Quantity(1),
                            sequence: SequenceNumber(taker),
                            side: Side::Buy,
                        },
                        &mut reports,
                    )
                    .expect("fifo head fill");
                debug_assert_eq!(summary.filled_quantity, Quantity(1));
                checksum ^= reports
                    .iter()
                    .next()
                    .map_or(0, |report| report.maker_order_id.0);
            }
        }
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    }
    let allocations_after = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(allocations_after, allocations_before, "fifo allocation");
    assert_eq!(
        deallocations_after, deallocations_before,
        "fifo deallocation"
    );
    samples_ns.sort_unstable();
    let total: u128 = samples_ns.iter().map(|sample| u128::from(*sample)).sum();
    let count = u128::try_from(SAMPLES).expect("sample count fits u128");
    let mean = total / count;
    let ops_per_second = 1_000_000_000_u128 * count / total.max(1);
    let book_bytes = core::mem::size_of::<OrderBook<1, DEPTH>>();
    println!(
        "fifo_bench scenario={} depth={DEPTH} samples={SAMPLES} p50_ns={} p90_ns={} p99_ns={} p99_9_ns={} max_ns={} mean_ns={mean} ops_per_second={ops_per_second} book_bytes={book_bytes} allocations=0 deallocations=0 checksum={checksum:016x}",
        scenario.name(),
        percentile(&samples_ns, 500),
        percentile(&samples_ns, 900),
        percentile(&samples_ns, 990),
        percentile(&samples_ns, 999),
        samples_ns[SAMPLES - 1],
    );
}

fn fifo_fixture<const DEPTH: usize>() -> OrderBook<1, DEPTH> {
    let mut book = OrderBook::<1, DEPTH>::new(InstrumentId(1));
    let mut reports = ReportBuffer::<1>::new();
    for id in 1..=u64::try_from(DEPTH).expect("depth fits u64") {
        book.submit(
            NewOrder {
                order_id: OrderId(id),
                account_id: AccountId(1),
                instrument_id: InstrumentId(1),
                price: PriceTicks(100),
                quantity: Quantity(1),
                sequence: SequenceNumber(id),
                side: Side::Sell,
            },
            &mut reports,
        )
        .expect("fixture order rests");
        reports.clear();
    }
    book
}

fn cancel_benchmark() {
    const LEVELS: usize = 512;
    const BATCHES: u64 = 1_024;
    let levels = u64::try_from(LEVELS).expect("level count fits u64");
    let operations = BATCHES * levels;
    let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
    let mut elapsed_ns = 0_u128;
    let mut checksum = 0_u64;

    for batch in 0..BATCHES {
        let mut book = OrderBook::<LEVELS, 1>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<1>::new();
        let first_id = batch * levels + 1;
        for level in 0..LEVELS {
            let offset = u64::try_from(level).expect("level index fits u64");
            let id = first_id + offset;
            book.submit(
                NewOrder {
                    order_id: OrderId(id),
                    account_id: AccountId(1),
                    instrument_id: InstrumentId(1),
                    price: PriceTicks(i64::try_from(level + 1).expect("price fits i64")),
                    quantity: Quantity(1),
                    sequence: SequenceNumber(id),
                    side: Side::Sell,
                },
                &mut reports,
            )
            .expect("benchmark order rests");
            reports.clear();
        }

        let started = Instant::now();
        for level in (0..LEVELS).rev() {
            let id = first_id + u64::try_from(level).expect("level index fits u64");
            let cancelled = book
                .cancel(CancelOrder {
                    order_id: OrderId(id),
                    account_id: AccountId(1),
                    instrument_id: InstrumentId(1),
                    sequence: SequenceNumber(id),
                })
                .expect("benchmark cancellation");
            checksum ^= cancelled.order_id.0;
        }
        elapsed_ns += started.elapsed().as_nanos();
        assert_eq!(book.order_count(), 0, "all benchmark orders cancelled");
    }

    let allocations_after = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(allocations_after, allocations_before, "cancel allocation");
    assert_eq!(
        deallocations_after, deallocations_before,
        "cancel deallocation"
    );
    let operations_u128 = u128::from(operations);
    let cancels_per_second = 1_000_000_000_u128 * operations_u128 / elapsed_ns;
    let book_bytes = core::mem::size_of::<OrderBook<LEVELS, 1>>();
    println!(
        "cancel_bench implementation=indexed cancels={operations} elapsed_ns={elapsed_ns} ns_per_cancel={} cancels_per_second={cancels_per_second} book_bytes={book_bytes} allocations=0 deallocations=0 checksum={checksum:016x}",
        elapsed_ns / operations_u128,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counting_allocator_tracks_round_trip() {
        let allocations_before = ALLOCATIONS.load(Ordering::SeqCst);
        let deallocations_before = DEALLOCATIONS.load(Ordering::SeqCst);
        let mut values = Vec::with_capacity(16);
        values.extend(0_u64..16);
        assert_eq!(values[15], 15);
        drop(values);
        assert!(ALLOCATIONS.load(Ordering::SeqCst) > allocations_before);
        assert!(DEALLOCATIONS.load(Ordering::SeqCst) > deallocations_before);
    }
}
