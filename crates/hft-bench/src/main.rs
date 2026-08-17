#![deny(unsafe_op_in_unsafe_fn)]

use hft_book::OrderBook;
use hft_gateway::Gateway;
use hft_io::RxFrame;
use hft_risk::{RiskEngine, RiskLimits};
use hft_spsc::SpscQueue;
use hft_types::{
    AccountId, CancelOrder, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, RejectReason,
    ReportBuffer, SequenceNumber, Side,
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
    // Sampling skips the first iterations so cold-start effects (frequency
    // ramp, first-touch stack pages, branch training) stay out of the
    // reported distribution. The total message sequence is unchanged, so the
    // final digest is comparable across runs and versions.
    const SAMPLE_AFTER: u64 = 1_000;
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
        let first_id = iteration * 2 + 3;
        if (SAMPLE_AFTER..SAMPLE_AFTER + (LATENCY_SAMPLES / 2) as u64).contains(&iteration) {
            let sample = usize::try_from(iteration - SAMPLE_AFTER).expect("sample index");
            let sell = encode_new_order(order(first_id, AccountId(1), Side::Sell));
            let sample_started = Instant::now();
            gateway
                .process_frame(&RxFrame::from_bytes(&sell), &mut reports)
                .expect("sampled sell");
            samples[sample * 2] =
                u64::try_from(sample_started.elapsed().as_nanos()).unwrap_or(u64::MAX);

            let buy = encode_new_order(order(first_id + 1, AccountId(2), Side::Buy));
            let sample_started = Instant::now();
            gateway
                .process_frame(&RxFrame::from_bytes(&buy), &mut reports)
                .expect("sampled buy");
            samples[sample * 2 + 1] =
                u64::try_from(sample_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        } else {
            run_pair(&mut gateway, &mut reports, first_id).expect("measured path");
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
    let stats = analyze(&mut samples);
    let (maximum_occupancy, backpressure_events) = queue_capacity_smoke();
    println!(
        "messages={messages} elapsed_ns={} mean_ns={} p50_ns={} p90_ns={} p99_ns={} p99_9_ns={} max_ns={} queue_max_occupancy={} backpressure_events={} allocations=0 deallocations=0 digest={:016x}",
        elapsed.as_nanos(),
        elapsed.as_nanos() / u128::from(messages),
        stats.p50,
        stats.p90,
        stats.p99,
        stats.p99_9,
        stats.max,
        maximum_occupancy,
        backpressure_events,
        gateway.stable_digest()
    );
    cancel_benchmark();
    fifo_benchmark();
    risk_benchmark();
    price_benchmark();
    match_plan_benchmark();
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

/// One timed fifo operation on a pre-built book. Book construction stays
/// outside the timed region.
fn fifo_op<const DEPTH: usize>(
    book: &mut OrderBook<1, DEPTH>,
    reports: &mut ReportBuffer<1>,
    scenario: FifoScenario,
    target: u64,
) -> u64 {
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
            cancelled.order_id.0
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
                    reports,
                )
                .expect("fifo head fill");
            debug_assert_eq!(summary.filled_quantity, Quantity(1));
            reports
                .iter()
                .next()
                .map_or(0, |report| report.maker_order_id.0)
        }
    }
}

fn run_fifo_cell<const DEPTH: usize>(scenario: FifoScenario) {
    const SAMPLES: usize = 2_000;
    const WARMUP: usize = 64;
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
    for _ in 0..WARMUP {
        let mut book = fifo_fixture::<DEPTH>();
        let mut reports = ReportBuffer::<1>::new();
        checksum ^= fifo_op(&mut book, &mut reports, scenario, target);
    }
    for sample in &mut samples_ns {
        let mut book = fifo_fixture::<DEPTH>();
        let mut reports = ReportBuffer::<1>::new();
        let started = Instant::now();
        let contribution = fifo_op(&mut book, &mut reports, scenario, target);
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        checksum ^= contribution;
    }
    let allocations_after = ALLOCATIONS.load(Ordering::SeqCst);
    let deallocations_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(allocations_after, allocations_before, "fifo allocation");
    assert_eq!(
        deallocations_after, deallocations_before,
        "fifo deallocation"
    );
    let stats = analyze(&mut samples_ns);
    let ops_per_second = 1_000_000_000_u128 / stats.mean.max(1);
    let book_bytes = core::mem::size_of::<OrderBook<1, DEPTH>>();
    println!(
        "fifo_bench scenario={} depth={DEPTH} samples={SAMPLES} p50_ns={} p90_ns={} p99_ns={} p99_9_ns={} max_ns={} mean_ns={} ops_per_second={ops_per_second} book_bytes={book_bytes} allocations=0 deallocations=0 checksum={checksum:016x}",
        scenario.name(),
        stats.p50,
        stats.p90,
        stats.p99,
        stats.p99_9,
        stats.max,
        stats.mean,
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

/// Engine at the stated reservation occupancy with 60 registered accounts.
fn risk_fixture(occupancy: u64, limits: RiskLimits) -> RiskEngine<64, 1024> {
    let mut risk = RiskEngine::<64, 1024>::new();
    for account in 1..=60 {
        risk.register_account(AccountId(account), limits)
            .expect("register account");
    }
    for id in 1..=occupancy {
        let side = if id % 2 == 0 { Side::Sell } else { Side::Buy };
        risk.check_and_reserve(NewOrder {
            order_id: OrderId(id),
            account_id: AccountId(u32::try_from((id % 60) + 1).expect("account id fits u32")),
            instrument_id: InstrumentId(1),
            price: PriceTicks(100),
            quantity: Quantity(1),
            sequence: SequenceNumber(id),
            side,
        })
        .expect("populate reservation");
    }
    risk
}

/// One reservation attempt shared by the risk benchmark loops.
fn reserve_one(
    risk: &mut RiskEngine<64, 1024>,
    id: u64,
    quantity: u64,
) -> Result<(), RejectReason> {
    let side = if id % 2 == 0 { Side::Sell } else { Side::Buy };
    risk.check_and_reserve(NewOrder {
        order_id: OrderId(id),
        account_id: AccountId(u32::try_from((id % 60) + 1).expect("account id fits u32")),
        instrument_id: InstrumentId(1),
        price: PriceTicks(100),
        quantity: Quantity(quantity),
        sequence: SequenceNumber(id),
        side,
    })
}

fn print_risk_stats(
    op: &str,
    index: &str,
    occupancy: u64,
    samples: &mut [u64],
    engine_bytes: usize,
) {
    let count = samples.len();
    let stats = analyze(samples);
    println!(
        "risk_bench op={op} index={index} occupancy={occupancy} samples={count} mean_ns={} p50_ns={} p90_ns={} p99_ns={} p99_9_ns={} max_ns={} engine_bytes={engine_bytes} allocations=0 deallocations=0 checksum=0",
        stats.mean, stats.p50, stats.p90, stats.p99, stats.p99_9, stats.max,
    );
}

#[allow(clippy::too_many_lines)]
fn risk_benchmark() {
    const SAMPLES: usize = 256;
    const WARMUP: u64 = 64;
    const ACCOUNT_CAPACITY: usize = 64;
    const ORDER_CAPACITY: usize = 1024;

    let alloc_before = ALLOCATIONS.load(Ordering::SeqCst);
    let dealloc_before = DEALLOCATIONS.load(Ordering::SeqCst);

    let wide = RiskLimits {
        max_quantity: Quantity(100_000),
        max_notional: 1_000_000_000_000,
        max_abs_position: Quantity(1_000_000),
        max_open_orders: 4096,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000_000),
    };
    let engine_bytes = core::mem::size_of::<RiskEngine<ACCOUNT_CAPACITY, ORDER_CAPACITY>>();

    // Reservation occupancy sweeps: 102, 512, 921 (≈10 %, 50 %, 90 % of 1024).
    // Every operation runs against a freshly populated engine so destructive
    // operations measure live reservations, not already-closed order IDs.
    // Destructive operations warm up on a margin band above the target
    // occupancy so the measured band starts at the stated occupancy.
    for &target in &[102_u64, 512, 921] {
        // risk_check: reserve fresh orders, starting at the target occupancy.
        // The monotonic-ID rule forbids reuse, so the sample count is capped
        // by the remaining order capacity after the warm-up band.
        {
            let budget = u64::try_from(ORDER_CAPACITY).expect("capacity fits u64") - target;
            let warm = WARMUP.min(budget / 4);
            let op_samples = u64::try_from(SAMPLES)
                .expect("samples fit u64")
                .min(budget - warm);
            let mut risk = risk_fixture(target, wide);
            for i in 0..warm {
                reserve_one(&mut risk, target + i + 1, 1).expect("warm-up reservation");
            }
            let mut samples = [0_u64; SAMPLES];
            let measured = usize::try_from(op_samples).expect("sample count fits usize");
            time_samples(&mut samples[..measured], |i| {
                reserve_one(&mut risk, target + warm + i + 1, 1).expect("measured reservation");
            });
            print_risk_stats(
                "risk_check",
                "reservation",
                target,
                &mut samples[..measured],
                engine_bytes,
            );
        }

        // reservation_lookup: probe live reservations without mutation.
        {
            let risk = risk_fixture(target, wide);
            for i in 0..WARMUP {
                let id = (i % target) + 1;
                let _ = risk.can_cancel(
                    OrderId(id),
                    AccountId(u32::try_from((id % 60) + 1).expect("account id fits u32")),
                );
            }
            let mut samples = [0_u64; SAMPLES];
            time_samples(&mut samples, |i| {
                let id = (i % target) + 1;
                let found = risk.can_cancel(
                    OrderId(id),
                    AccountId(u32::try_from((id % 60) + 1).expect("account id fits u32")),
                );
                debug_assert!(found.is_ok(), "lookup targets a live reservation");
            });
            print_risk_stats(
                "reservation_lookup",
                "reservation",
                target,
                &mut samples,
                engine_bytes,
            );
        }

        // fill, cancel, and settle are destructive: each sample consumes one
        // live reservation, so the sample count is capped by the occupancy.
        let live_samples = SAMPLES.min(usize::try_from(target).expect("target fits usize"));

        // fill: terminal one-unit fills of live reservations.
        {
            let mut risk = risk_fixture(target + WARMUP, wide);
            for i in 0..WARMUP {
                let id = target + WARMUP - i;
                risk.record_fill(OrderId(id), Quantity(1))
                    .expect("warm-up fill");
            }
            let mut samples = [0_u64; SAMPLES];
            time_samples(&mut samples[..live_samples], |i| {
                let filled = risk.record_fill(OrderId(target - i), Quantity(1));
                debug_assert!(filled.is_ok(), "fill targets a live reservation");
            });
            print_risk_stats(
                "fill",
                "reservation",
                target,
                &mut samples[..live_samples],
                engine_bytes,
            );
        }

        // cancel: release live reservations.
        {
            let mut risk = risk_fixture(target + WARMUP, wide);
            for i in 0..WARMUP {
                let id = target + WARMUP - i;
                risk.cancel_reservation(
                    OrderId(id),
                    AccountId(u32::try_from((id % 60) + 1).expect("account id fits u32")),
                )
                .expect("warm-up cancel");
            }
            let mut samples = [0_u64; SAMPLES];
            time_samples(&mut samples[..live_samples], |i| {
                let id = target - i;
                let released = risk.cancel_reservation(
                    OrderId(id),
                    AccountId(u32::try_from((id % 60) + 1).expect("account id fits u32")),
                );
                debug_assert!(released.is_ok(), "cancel targets a live reservation");
            });
            print_risk_stats(
                "cancel",
                "reservation",
                target,
                &mut samples[..live_samples],
                engine_bytes,
            );
        }

        // settle: settle live reservations with their full filled quantity.
        {
            let mut risk = risk_fixture(target + WARMUP, wide);
            for i in 0..WARMUP {
                let id = target + WARMUP - i;
                risk.settle(OrderId(id), Quantity(1))
                    .expect("warm-up settle");
            }
            let mut samples = [0_u64; SAMPLES];
            time_samples(&mut samples[..live_samples], |i| {
                let settled = risk.settle(OrderId(target - i), Quantity(1));
                debug_assert!(settled.is_ok(), "settle targets a live reservation");
            });
            print_risk_stats(
                "settle",
                "reservation",
                target,
                &mut samples[..live_samples],
                engine_bytes,
            );
        }

        // reject: oversized quantity fails the limit check without mutation.
        {
            let mut risk = risk_fixture(target, wide);
            for i in 0..WARMUP {
                let _ = reserve_one(&mut risk, target + i + 20_000, 100_001);
            }
            let mut samples = [0_u64; SAMPLES];
            time_samples(&mut samples, |i| {
                let rejected = reserve_one(&mut risk, target + i + 30_000, 100_001);
                debug_assert_eq!(rejected, Err(RejectReason::QuantityLimit));
            });
            print_risk_stats("reject", "reservation", target, &mut samples, engine_bytes);
        }
    }

    // Account index occupancy sweeps: 6, 32, 57 (≈10 %, 50 %, 90 % of 64).
    for &target in &[6_u64, 32, 57] {
        let mut risk = RiskEngine::<ACCOUNT_CAPACITY, ORDER_CAPACITY>::new();
        for acct in 1..=target {
            risk.register_account(
                AccountId(u32::try_from(acct).expect("account id fits u32")),
                wide,
            )
            .expect("register account");
        }
        for i in 0..WARMUP {
            let acct = u32::try_from((i % target) + 1).expect("account id fits u32");
            let _ = risk.account_snapshot(AccountId(acct));
        }
        let mut samples = [0_u64; SAMPLES];
        time_samples(&mut samples, |i| {
            let acct = u32::try_from((i % target) + 1).expect("account id fits u32");
            let _ = risk.account_snapshot(AccountId(acct));
        });
        print_risk_stats(
            "account_lookup",
            "account",
            target,
            &mut samples,
            engine_bytes,
        );
    }

    let alloc_after = ALLOCATIONS.load(Ordering::SeqCst);
    let dealloc_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(alloc_after, alloc_before, "risk bench allocation");
    assert_eq!(dealloc_after, dealloc_before, "risk bench deallocation");
}

fn bench_order(id: u64, account: u32, price: i64, quantity: u64, side: Side) -> NewOrder {
    NewOrder {
        order_id: OrderId(id),
        account_id: AccountId(account),
        instrument_id: InstrumentId(1),
        price: PriceTicks(price),
        quantity: Quantity(quantity),
        sequence: SequenceNumber(id),
        side,
    }
}

fn print_price_stats(op: &str, scenario: &str, samples: &mut [u64]) {
    let count = samples.len();
    let stats = analyze(samples);
    println!(
        "price_bench op={op} scenario={scenario} samples={count} mean_ns={} p50_ns={} p90_ns={} p99_ns={} p99_9_ns={} max_ns={} allocations=0 deallocations=0",
        stats.mean, stats.p50, stats.p90, stats.p99, stats.p99_9, stats.max,
    );
}

fn price_benchmark() {
    let alloc_before = ALLOCATIONS.load(Ordering::SeqCst);
    let dealloc_before = DEALLOCATIONS.load(Ordering::SeqCst);

    price_scenario::<64, 1>("dense_64_32", 32);
    price_scenario::<128, 1>("sparse_128_16", 16);
    price_scenario::<128, 1>("dense_128_64", 64);
    price_scenario::<128, 1>("dense_128_120", 120);

    let alloc_after = ALLOCATIONS.load(Ordering::SeqCst);
    let dealloc_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(alloc_after, alloc_before, "price bench allocation");
    assert_eq!(dealloc_after, dealloc_before, "price bench deallocation");
}

/// Every scenario keeps its book in a steady state: each timed operation is
/// balanced by an untimed operation that restores the starting shape, so all
/// samples measure the intended operation at the stated occupancy.
#[allow(clippy::too_many_lines)]
fn price_scenario<const L: usize, const O: usize>(name: &str, active: usize) {
    const SAMPLES: usize = 2_000;
    const WARMUP: u64 = 64;

    // submit_cross: a one-unit buy crosses the best ask; the consumed maker
    // is replaced untimed, so every sample crosses a full book.
    {
        let mut book = OrderBook::<L, O>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<1>::new();
        for i in 0..active {
            let id = u64::try_from(i + 1).expect("id fits u64");
            let price = 100 + i64::try_from(i).expect("price fits i64");
            book.submit(bench_order(id, 1, price, 1, Side::Sell), &mut reports)
                .expect("rest sell level");
            reports.clear();
        }
        // The consumed level is always the best ask; its price is read from
        // the fill report and restored untimed, keeping the book full. Only
        // the crossing submit is timed; the replenish is the teardown.
        let primary = |book: &mut OrderBook<L, O>, reports: &mut ReportBuffer<1>, i: u64| {
            let summary = book
                .submit(bench_order(1_000_000 + i, 2, 1_000, 1, Side::Buy), reports)
                .expect("cross best ask");
            debug_assert_eq!(summary.filled_quantity, Quantity(1));
            let consumed_price = reports.iter().next().expect("one fill").price.0;
            reports.clear();
            consumed_price
        };
        let teardown =
            |book: &mut OrderBook<L, O>, reports: &mut ReportBuffer<1>, i: u64, price: i64| {
                book.submit(bench_order(2_000_000 + i, 1, price, 1, Side::Sell), reports)
                    .expect("replenish consumed level");
                reports.clear();
            };
        for i in 0..WARMUP {
            let price = primary(&mut book, &mut reports, i);
            teardown(&mut book, &mut reports, i, price);
        }
        let mut samples = [0_u64; SAMPLES];
        for (index, sample) in samples.iter_mut().enumerate() {
            let i = WARMUP + u64::try_from(index).expect("sample index fits u64");
            let started = Instant::now();
            let price = primary(&mut book, &mut reports, i);
            *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            teardown(&mut book, &mut reports, i, price);
        }
        print_price_stats("submit_cross", name, &mut samples);
    }

    // discovery: deep makers never deplete, so a one-unit buy at the best
    // price measures pure best-price discovery plus a partial fill.
    {
        let mut book = OrderBook::<L, O>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<1>::new();
        for i in 0..active {
            let id = u64::try_from(i + 1).expect("id fits u64");
            let price = 100 + i64::try_from(i).expect("price fits i64");
            book.submit(bench_order(id, 1, price, 100_000, Side::Sell), &mut reports)
                .expect("rest sell");
            reports.clear();
        }
        let discover = |book: &mut OrderBook<L, O>, reports: &mut ReportBuffer<1>, i: u64| {
            let taker = bench_order(1_000_000 + i, 2, 100, 1, Side::Buy);
            let summary = book.submit(taker, reports).expect("discover best ask");
            debug_assert_eq!(summary.filled_quantity, Quantity(1));
            reports.clear();
        };
        for i in 0..WARMUP {
            discover(&mut book, &mut reports, i);
        }
        let mut samples = [0_u64; SAMPLES];
        time_samples(&mut samples, |i| {
            discover(&mut book, &mut reports, WARMUP + i);
        });
        print_price_stats("discovery", name, &mut samples);
    }

    // level_create: a timed rest into a new price level, then an untimed
    // cancel returns the slot, so every sample creates a level at the same
    // occupancy.
    {
        let mut book = OrderBook::<L, O>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<1>::new();
        for i in 0..active.min(8) {
            let id = u64::try_from(i + 1).expect("id fits u64");
            let price = 200 + i64::try_from(i).expect("price fits i64");
            book.submit(bench_order(id, 1, price, 1, Side::Sell), &mut reports)
                .expect("pre-populate");
            reports.clear();
        }
        // Every rep creates a level at the same price (same sorted position)
        // and cancels it untimed, so all samples measure one creation. Only
        // the resting submit is timed; the cancel is the teardown.
        let primary = |book: &mut OrderBook<L, O>, reports: &mut ReportBuffer<1>, i: u64| {
            let id = 3_000_000 + i;
            book.submit(bench_order(id, 1, 500, 1, Side::Sell), reports)
                .expect("create level");
            reports.clear();
        };
        let teardown = |book: &mut OrderBook<L, O>, i: u64| {
            let id = 3_000_000 + i;
            book.cancel(CancelOrder {
                order_id: OrderId(id),
                account_id: AccountId(1),
                instrument_id: InstrumentId(1),
                sequence: SequenceNumber(id),
            })
            .expect("remove created level");
        };
        for i in 0..WARMUP {
            primary(&mut book, &mut reports, i);
            teardown(&mut book, i);
        }
        let mut samples = [0_u64; SAMPLES];
        for (index, sample) in samples.iter_mut().enumerate() {
            let i = WARMUP + u64::try_from(index).expect("sample index fits u64");
            let started = Instant::now();
            primary(&mut book, &mut reports, i);
            *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            teardown(&mut book, i);
        }
        print_price_stats("level_create", name, &mut samples);
    }
}

fn print_plan_stats(scenario: &str, shape: &str, samples: &mut [u64], checksum: u64) {
    let count = samples.len();
    let stats = analyze(samples);
    println!(
        "match_plan_bench scenario={scenario} samples={count} mean_ns={} p50_ns={} p90_ns={} p99_ns={} p99_9_ns={} max_ns={} {shape} allocations=0 deallocations=0 checksum={checksum:016x}",
        stats.mean, stats.p50, stats.p90, stats.p99, stats.p99_9, stats.max,
    );
}

#[allow(clippy::too_many_lines)]
fn match_plan_benchmark() {
    const SAMPLES: usize = 2_000;
    const WARMUP: u64 = 64;
    let alloc_before = ALLOCATIONS.load(Ordering::SeqCst);
    let dealloc_before = DEALLOCATIONS.load(Ordering::SeqCst);

    // Non-crossing: taker rests without walking the plan; the rest is
    // cancelled untimed so the book shape is constant across samples.
    {
        let mut book = OrderBook::<128, 8>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<8>::new();
        book.submit(bench_order(1, 1, 100, 10_000, Side::Sell), &mut reports)
            .expect("rest ask");
        reports.clear();
        let mut checksum = 0_u64;
        let primary = |book: &mut OrderBook<128, 8>, reports: &mut ReportBuffer<8>, i: u64| {
            let summary = book
                .submit(bench_order(1_000_000 + i, 2, 99, 1, Side::Buy), reports)
                .expect("non-crossing rests");
            reports.clear();
            summary.filled_quantity.0
        };
        let teardown = |book: &mut OrderBook<128, 8>, i: u64| {
            let id = 1_000_000 + i;
            book.cancel(CancelOrder {
                order_id: OrderId(id),
                account_id: AccountId(2),
                instrument_id: InstrumentId(1),
                sequence: SequenceNumber(id),
            })
            .expect("rested bid cancels");
        };
        for i in 0..WARMUP {
            checksum ^= primary(&mut book, &mut reports, i);
            teardown(&mut book, i);
        }
        let mut samples = [0_u64; SAMPLES];
        for (index, sample) in samples.iter_mut().enumerate() {
            let i = WARMUP + u64::try_from(index).expect("sample index fits u64");
            let started = Instant::now();
            checksum ^= primary(&mut book, &mut reports, i);
            *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            teardown(&mut book, i);
        }
        print_plan_stats(
            "non_crossing",
            "traversals=0 fills=0 reports=0",
            &mut samples,
            checksum,
        );
    }

    // Single fill: one deep maker crosses at the best price and never
    // depletes across the sample count.
    {
        let mut book = OrderBook::<128, 8>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<8>::new();
        book.submit(bench_order(1, 1, 100, 10_000_000, Side::Sell), &mut reports)
            .expect("rest ask");
        reports.clear();
        let mut checksum = 0_u64;
        let mut fill_once =
            |book: &mut OrderBook<128, 8>, reports: &mut ReportBuffer<8>, i: u64| {
                let summary = book
                    .submit(bench_order(1_000_000 + i, 2, 100, 1, Side::Buy), reports)
                    .expect("single fill");
                checksum ^= summary.filled_quantity.0;
                reports.clear();
            };
        for i in 0..WARMUP {
            fill_once(&mut book, &mut reports, i);
        }
        let mut samples = [0_u64; SAMPLES];
        time_samples(&mut samples, |i| {
            fill_once(&mut book, &mut reports, WARMUP + i);
        });
        print_plan_stats(
            "single_fill",
            "traversals=1 fills=1 reports=1",
            &mut samples,
            checksum,
        );
    }

    // Multi fill: a taker crosses eight deep makers across levels.
    {
        let mut book = OrderBook::<128, 8>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<8>::new();
        for i in 0..8_u64 {
            let price = 100 + i64::try_from(i).expect("price fits i64");
            book.submit(
                bench_order(i + 1, 1, price, 1_000_000, Side::Sell),
                &mut reports,
            )
            .expect("rest ask level");
            reports.clear();
        }
        let mut checksum = 0_u64;
        let mut cross_eight =
            |book: &mut OrderBook<128, 8>, reports: &mut ReportBuffer<8>, i: u64| {
                let summary = book
                    .submit(bench_order(1_000_000 + i, 2, 1_000, 8, Side::Buy), reports)
                    .expect("multi fill");
                checksum ^= summary.filled_quantity.0;
                reports.clear();
            };
        for i in 0..WARMUP {
            cross_eight(&mut book, &mut reports, i);
        }
        let mut samples = [0_u64; SAMPLES];
        time_samples(&mut samples, |i| {
            cross_eight(&mut book, &mut reports, WARMUP + i);
        });
        print_plan_stats(
            "multi_fill",
            "traversals=8 fills=8 reports=8",
            &mut samples,
            checksum,
        );
    }

    // Report-full rejection: a taker that would exceed report capacity is
    // rejected atomically by the plan preflight.
    {
        let mut book = OrderBook::<128, 8>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<8>::new();
        for i in 0..9_u64 {
            let price = 100 + i64::try_from(i).expect("price fits i64");
            book.submit(bench_order(i + 1, 1, price, 1, Side::Sell), &mut reports)
                .expect("rest ask level");
            reports.clear();
        }
        let reject_once = |book: &mut OrderBook<128, 8>, reports: &mut ReportBuffer<8>, i: u64| {
            let rejected = book
                .submit(bench_order(1_000_000 + i, 2, 1_000, 9, Side::Buy), reports)
                .expect_err("report capacity rejected");
            debug_assert!(matches!(rejected, RejectReason::ReportCapacity));
        };
        for i in 0..WARMUP {
            reject_once(&mut book, &mut reports, i);
        }
        let mut samples = [0_u64; SAMPLES];
        time_samples(&mut samples, |i| {
            reject_once(&mut book, &mut reports, WARMUP + i);
        });
        print_plan_stats(
            "report_full",
            "traversals=9 fills=0 reports=0",
            &mut samples,
            0,
        );
    }

    // Deep rejection: a taker trying to rest into a full best-priced level is
    // rejected without mutating the book.
    {
        let mut book = OrderBook::<128, 8>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<8>::new();
        for i in 0..8_u64 {
            book.submit(bench_order(i + 1, 1, 100, 1, Side::Sell), &mut reports)
                .expect("rest ask at best price");
            reports.clear();
        }
        let reject_once = |book: &mut OrderBook<128, 8>, reports: &mut ReportBuffer<8>, i: u64| {
            let rejected = book
                .submit(bench_order(1_000_000 + i, 1, 100, 1, Side::Sell), reports)
                .expect_err("full best level rejected");
            debug_assert!(matches!(rejected, RejectReason::PriceLevelOrderCapacity));
        };
        for i in 0..WARMUP {
            reject_once(&mut book, &mut reports, i);
        }
        let mut samples = [0_u64; SAMPLES];
        time_samples(&mut samples, |i| {
            reject_once(&mut book, &mut reports, WARMUP + i);
        });
        print_plan_stats(
            "deep_rejection",
            "traversals=1 fills=0 reports=0",
            &mut samples,
            0,
        );
    }

    let alloc_after = ALLOCATIONS.load(Ordering::SeqCst);
    let dealloc_after = DEALLOCATIONS.load(Ordering::SeqCst);
    assert_eq!(alloc_after, alloc_before, "match plan bench allocation");
    assert_eq!(
        dealloc_after, dealloc_before,
        "match plan bench deallocation"
    );
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

    // Batch 0 is an untimed warm-up so the timed batches run hot.
    for batch in 0..=BATCHES {
        let mut book = OrderBook::<LEVELS, 1>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<1>::new();
        let first_id = batch * levels + 1;
        for level in 0..LEVELS {
            let offset = u64::try_from(level).expect("level index fits u64");
            let id = first_id + offset;
            let price = i64::try_from(level + 1).expect("price fits i64");
            book.submit(bench_order(id, 1, price, 1, Side::Sell), &mut reports)
                .expect("benchmark order rests");
            reports.clear();
        }

        let started = Instant::now();
        let mut batch_checksum = 0_u64;
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
            batch_checksum ^= cancelled.order_id.0;
        }
        let batch_ns = started.elapsed().as_nanos();
        assert_eq!(book.order_count(), 0, "all benchmark orders cancelled");
        if batch > 0 {
            elapsed_ns += batch_ns;
            checksum ^= batch_checksum;
        }
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

/// Times `op` once per sample, storing per-sample nanoseconds.
fn time_samples(samples: &mut [u64], mut op: impl FnMut(u64)) {
    for (index, sample) in samples.iter_mut().enumerate() {
        let index = u64::try_from(index).expect("sample index fits u64");
        let started = Instant::now();
        op(index);
        *sample = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    }
}

/// Percentiles and mean for one sample set. Sorts in place. The mean is
/// reported for continuity but is outlier-sensitive; p50-p99.9 are the robust
/// shape, and max records the worst observed scheduler interference.
struct SampleStats {
    mean: u128,
    p50: u64,
    p90: u64,
    p99: u64,
    p99_9: u64,
    max: u64,
}

fn analyze(samples: &mut [u64]) -> SampleStats {
    debug_assert!(!samples.is_empty());
    samples.sort_unstable();
    let total: u128 = samples.iter().map(|sample| u128::from(*sample)).sum();
    let count = u128::try_from(samples.len()).expect("sample count fits u128");
    SampleStats {
        mean: total / count,
        p50: percentile(samples, 500),
        p90: percentile(samples, 900),
        p99: percentile(samples, 990),
        p99_9: percentile(samples, 999),
        max: samples[samples.len() - 1],
    }
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
