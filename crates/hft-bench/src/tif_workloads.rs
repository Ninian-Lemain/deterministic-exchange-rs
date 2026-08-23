//! Time-in-force workloads: IOC and FOK submission paths timed against
//! equivalent GTC paths on one mid-price book. Every scenario rests its
//! fixture makers up front, warms up untimed, gates allocations around the
//! sampling window, and restores the steady state with untimed teardown work
//! between samples.

use crate::record::{BenchRecord, Extra};
use crate::{ALLOCATIONS, DEALLOCATIONS, analyze};
use hft_book::OrderBook;
use hft_types::{
    AccountId, CancelOrder, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, RejectReason,
    ReportBuffer, SequenceNumber, Side, TimeInForce,
};
use std::sync::atomic::Ordering;
use std::time::Instant;

/// Book shape shared by every time-in-force scenario.
type BenchBook = OrderBook<128, 8>;

/// Report capacity shared by every scenario.
type Reports = ReportBuffer<16>;

const INSTRUMENT: InstrumentId = InstrumentId(1);
const MAKER_ACCOUNT: AccountId = AccountId(1);
const TAKER_ACCOUNT: AccountId = AccountId(2);

/// Untimed iterations per scenario before sampling starts.
const WARMUP_SAMPLES: usize = 64;

/// Checksum contribution for scenarios whose submit rejects.
const REJECT_CHECKSUM: u64 = 0x0F0F_F00D;

/// Measures IOC and FOK submit latency against equivalent GTC paths, one
/// record per scenario. Only the submit call is timed; fixture repair and
/// replenishment stay outside the clock, so each sample observes the intended
/// steady-state occupancy.
///
/// # Panics
///
/// Panics when a fixture assertion fails, a fill quantity deviates from the
/// expected amount, or an allocation gate trips between the pre-sampling
/// snapshot and the end of a sampling window.
pub fn tif_benchmark(samples: usize, out: &mut Vec<BenchRecord>) {
    if samples == 0 {
        return;
    }
    let mut next_taker_id = 1_000_000_u64;
    let mut replenish_id = 5_000_000_u64;
    run_ioc_empty(samples, &mut next_taker_id, &mut replenish_id, out);
    run_ioc_partial(samples, &mut next_taker_id, &mut replenish_id, out);
    run_ioc_full(samples, &mut next_taker_id, &mut replenish_id, out);
    run_fok_reject(samples, &mut next_taker_id, &mut replenish_id, out);
    run_fok_single(samples, &mut next_taker_id, &mut replenish_id, out);
    run_fok_multi(samples, &mut next_taker_id, &mut replenish_id, out);
    run_gtc_full(samples, &mut next_taker_id, &mut replenish_id, out);
    run_gtc_partial_rests(samples, &mut next_taker_id, &mut replenish_id, out);
}

/// Builds a taker or maker order with the sequence number tracking the id.
fn order(
    id: u64,
    account: AccountId,
    price: i64,
    qty: u64,
    side: Side,
    tif: TimeInForce,
) -> NewOrder {
    NewOrder {
        time_in_force: tif,
        order_id: OrderId(id),
        account_id: account,
        instrument_id: INSTRUMENT,
        price: PriceTicks(price),
        quantity: Quantity(qty),
        sequence: SequenceNumber(id),
        side,
    }
}

/// Draws the next monotonic id from a counter starting at its initial value.
fn take_id(counter: &mut u64) -> u64 {
    let id = *counter;
    *counter += 1;
    id
}

/// Rests one GTC maker on the ask side and resets the report buffer.
fn rest_fixture(
    book: &mut BenchBook,
    reports: &mut Reports,
    replenish_id: &mut u64,
    price: i64,
    qty: u64,
) {
    let id = take_id(replenish_id);
    book.submit(
        order(id, MAKER_ACCOUNT, price, qty, Side::Sell, TimeInForce::Gtc),
        reports,
    )
    .expect("fixture maker rests");
    reports.clear();
}

/// Loads both allocation counters immediately before a sampling window.
fn snapshot_gate() -> (u64, u64) {
    (
        ALLOCATIONS.load(Ordering::SeqCst),
        DEALLOCATIONS.load(Ordering::SeqCst),
    )
}

/// Asserts the allocation counters are unchanged across one sampling window.
fn assert_gate(before: (u64, u64), after: (u64, u64), scenario: &'static str) {
    assert_eq!(after.0, before.0, "{scenario}: allocations");
    assert_eq!(after.1, before.1, "{scenario}: deallocations");
}

/// Computes latency statistics over the sample slice, completes the pending
/// record fields, and appends the record.
fn finish(
    out: &mut Vec<BenchRecord>,
    mut record: BenchRecord,
    checksum: u64,
    latencies: &mut [u64],
) {
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
    record.checksum = checksum;
    out.push(record);
}

/// Stages consumed prices on the stack, clears the report buffer, then
/// re-rests one unit at each consumed price with a fresh maker id.
fn replenish_fan(book: &mut BenchBook, reports: &mut Reports, replenish_id: &mut u64) {
    let mut staged_prices = [0_i64; 16];
    let staged = reports.len().min(staged_prices.len());
    for (slot, report) in staged_prices.iter_mut().zip(reports.iter()) {
        *slot = report.price.0;
    }
    reports.clear();
    for &price in &staged_prices[..staged] {
        rest_fixture(book, reports, replenish_id, price, 1);
    }
}

/// Cancels the rested GTC remainder, then replenishes the consumed maker.
fn teardown_gtc_partial(
    book: &mut BenchBook,
    reports: &mut Reports,
    rested_bid: u64,
    replenish_id: &mut u64,
) {
    book.cancel(CancelOrder {
        order_id: OrderId(rested_bid),
        account_id: TAKER_ACCOUNT,
        instrument_id: INSTRUMENT,
        sequence: SequenceNumber(rested_bid),
    })
    .expect("rested GTC remainder cancels");
    rest_fixture(book, reports, replenish_id, 100, 3);
}

/// Times one IOC probe priced below the market: nothing fills, nothing rests.
fn step_ioc_empty(
    book: &mut BenchBook,
    reports: &mut Reports,
    next_taker_id: &mut u64,
    checksum: &mut u64,
) -> u128 {
    let id = take_id(next_taker_id);
    let started = Instant::now();
    let submitted = book.submit(
        order(id, TAKER_ACCOUNT, 99, 10, Side::Buy, TimeInForce::Ioc),
        reports,
    );
    let elapsed = started.elapsed().as_nanos();
    let summary = submitted.expect("below-market IOC accepts");
    assert_eq!(
        summary.filled_quantity.0, 0,
        "below-market IOC fills nothing"
    );
    assert_eq!(summary.report_count, 0, "below-market IOC emits no reports");
    *checksum ^= summary.filled_quantity.0;
    elapsed
}

/// Times one IOC crossing a shallow ask for a partial fill.
fn step_ioc_partial(
    book: &mut BenchBook,
    reports: &mut Reports,
    next_taker_id: &mut u64,
    checksum: &mut u64,
) -> u128 {
    let id = take_id(next_taker_id);
    let started = Instant::now();
    let submitted = book.submit(
        order(id, TAKER_ACCOUNT, 100, 5, Side::Buy, TimeInForce::Ioc),
        reports,
    );
    let elapsed = started.elapsed().as_nanos();
    let summary = submitted.expect("IOC partial submits");
    assert_eq!(summary.filled_quantity.0, 3, "IOC partial fills the ask");
    assert_eq!(summary.resting_quantity.0, 0, "IOC remainder never rests");
    *checksum ^= summary.filled_quantity.0;
    elapsed
}

/// Times one IOC filling four units from a deep ask.
fn step_ioc_full(
    book: &mut BenchBook,
    reports: &mut Reports,
    next_taker_id: &mut u64,
    checksum: &mut u64,
) -> u128 {
    let id = take_id(next_taker_id);
    let started = Instant::now();
    let submitted = book.submit(
        order(id, TAKER_ACCOUNT, 100, 4, Side::Buy, TimeInForce::Ioc),
        reports,
    );
    let elapsed = started.elapsed().as_nanos();
    let summary = submitted.expect("IOC full submits");
    assert_eq!(
        summary.filled_quantity.0, 4,
        "IOC fills the requested units"
    );
    assert_eq!(summary.resting_quantity.0, 0, "IOC remainder never rests");
    *checksum ^= summary.filled_quantity.0;
    elapsed
}

/// Times one FOK rejection for insufficient liquidity; the book is untouched.
fn step_fok_reject(
    book: &mut BenchBook,
    reports: &mut Reports,
    next_taker_id: &mut u64,
    checksum: &mut u64,
) -> u128 {
    let id = take_id(next_taker_id);
    let started = Instant::now();
    let submitted = book.submit(
        order(id, TAKER_ACCOUNT, 100, 5, Side::Buy, TimeInForce::Fok),
        reports,
    );
    let elapsed = started.elapsed().as_nanos();
    let rejected = submitted.expect_err("undersized FOK rejects");
    assert!(
        matches!(rejected, RejectReason::InsufficientLiquidity),
        "FOK rejects for missing liquidity"
    );
    assert_eq!(
        book.order_count(),
        1,
        "rejected FOK leaves the maker resting"
    );
    *checksum ^= REJECT_CHECKSUM;
    elapsed
}

/// Times one FOK filling a single unit from a deep ask.
fn step_fok_single(
    book: &mut BenchBook,
    reports: &mut Reports,
    next_taker_id: &mut u64,
    checksum: &mut u64,
) -> u128 {
    let id = take_id(next_taker_id);
    let started = Instant::now();
    let submitted = book.submit(
        order(id, TAKER_ACCOUNT, 100, 1, Side::Buy, TimeInForce::Fok),
        reports,
    );
    let elapsed = started.elapsed().as_nanos();
    let summary = submitted.expect("FOK single submits");
    assert_eq!(summary.filled_quantity.0, 1, "FOK single fills one unit");
    *checksum ^= summary.filled_quantity.0;
    elapsed
}

/// Times one FOK sweeping eight single-unit asks across eight price levels.
fn step_fok_multi(
    book: &mut BenchBook,
    reports: &mut Reports,
    next_taker_id: &mut u64,
    checksum: &mut u64,
) -> u128 {
    let id = take_id(next_taker_id);
    let started = Instant::now();
    let submitted = book.submit(
        order(id, TAKER_ACCOUNT, 1_000, 8, Side::Buy, TimeInForce::Fok),
        reports,
    );
    let elapsed = started.elapsed().as_nanos();
    let summary = submitted.expect("FOK multi sweeps the fan");
    assert_eq!(summary.filled_quantity.0, 8, "FOK multi fills eight units");
    assert_eq!(summary.report_count, 8, "one report per consumed level");
    *checksum ^= summary.filled_quantity.0;
    elapsed
}

/// Times one GTC filling four units from a deep ask, matching `ioc_full`.
fn step_gtc_full(
    book: &mut BenchBook,
    reports: &mut Reports,
    next_taker_id: &mut u64,
    checksum: &mut u64,
) -> u128 {
    let id = take_id(next_taker_id);
    let started = Instant::now();
    let submitted = book.submit(
        order(id, TAKER_ACCOUNT, 100, 4, Side::Buy, TimeInForce::Gtc),
        reports,
    );
    let elapsed = started.elapsed().as_nanos();
    let summary = submitted.expect("GTC full submits");
    assert_eq!(summary.filled_quantity.0, 4, "GTC full fills the ask units");
    assert_eq!(
        summary.resting_quantity.0, 0,
        "fully filled GTC never rests"
    );
    *checksum ^= summary.filled_quantity.0;
    elapsed
}

/// Times one GTC partially filling and resting the remainder as a bid.
/// Returns the elapsed nanoseconds and the rested bid id for the teardown.
fn step_gtc_partial_rests(
    book: &mut BenchBook,
    reports: &mut Reports,
    next_taker_id: &mut u64,
    checksum: &mut u64,
) -> (u128, u64) {
    let id = take_id(next_taker_id);
    let started = Instant::now();
    let submitted = book.submit(
        order(id, TAKER_ACCOUNT, 100, 5, Side::Buy, TimeInForce::Gtc),
        reports,
    );
    let elapsed = started.elapsed().as_nanos();
    let summary = submitted.expect("GTC partial submits");
    assert_eq!(summary.filled_quantity.0, 3, "GTC partial fills the ask");
    assert_eq!(
        summary.resting_quantity.0, 2,
        "GTC remainder rests as a bid"
    );
    *checksum ^= summary.filled_quantity.0;
    (elapsed, id)
}

/// Below-market IOC probes against one deep ask: zero-fill fast path.
fn run_ioc_empty(
    samples: usize,
    next_taker_id: &mut u64,
    replenish_id: &mut u64,
    out: &mut Vec<BenchRecord>,
) {
    let mut book = std::boxed::Box::new(BenchBook::new(INSTRUMENT));
    let mut reports = Reports::new();
    rest_fixture(&mut book, &mut reports, replenish_id, 100, 10_000_000);
    let mut checksum = 0_u64;
    for _ in 0..WARMUP_SAMPLES {
        step_ioc_empty(&mut book, &mut reports, next_taker_id, &mut checksum);
        reports.clear();
    }
    let mut latencies = vec![0_u64; samples];
    let before = snapshot_gate();
    for sample in &mut latencies {
        let elapsed = step_ioc_empty(&mut book, &mut reports, next_taker_id, &mut checksum);
        *sample = u64::try_from(elapsed).unwrap_or(u64::MAX);
        reports.clear();
    }
    let after = snapshot_gate();
    assert_gate(before, after, "tif ioc_empty");
    finish(
        out,
        BenchRecord::new(
            "component",
            "book",
            "ioc_empty",
            &[("tif", Extra::Text("ioc"))],
        ),
        checksum,
        &mut latencies,
    );
}

/// IOC partial fills against a three-unit ask restored between samples.
fn run_ioc_partial(
    samples: usize,
    next_taker_id: &mut u64,
    replenish_id: &mut u64,
    out: &mut Vec<BenchRecord>,
) {
    let mut book = std::boxed::Box::new(BenchBook::new(INSTRUMENT));
    let mut reports = Reports::new();
    rest_fixture(&mut book, &mut reports, replenish_id, 100, 3);
    let mut checksum = 0_u64;
    for _ in 0..WARMUP_SAMPLES {
        step_ioc_partial(&mut book, &mut reports, next_taker_id, &mut checksum);
        rest_fixture(&mut book, &mut reports, replenish_id, 100, 3);
    }
    let mut latencies = vec![0_u64; samples];
    let before = snapshot_gate();
    for sample in &mut latencies {
        let elapsed = step_ioc_partial(&mut book, &mut reports, next_taker_id, &mut checksum);
        *sample = u64::try_from(elapsed).unwrap_or(u64::MAX);
        rest_fixture(&mut book, &mut reports, replenish_id, 100, 3);
    }
    let after = snapshot_gate();
    assert_gate(before, after, "tif ioc_partial");
    finish(
        out,
        BenchRecord::new(
            "component",
            "book",
            "ioc_partial",
            &[("tif", Extra::Text("ioc"))],
        ),
        checksum,
        &mut latencies,
    );
}

/// IOC full fills from a deep ask that never depletes.
fn run_ioc_full(
    samples: usize,
    next_taker_id: &mut u64,
    replenish_id: &mut u64,
    out: &mut Vec<BenchRecord>,
) {
    let mut book = std::boxed::Box::new(BenchBook::new(INSTRUMENT));
    let mut reports = Reports::new();
    rest_fixture(&mut book, &mut reports, replenish_id, 100, 10_000_000);
    let mut checksum = 0_u64;
    for _ in 0..WARMUP_SAMPLES {
        step_ioc_full(&mut book, &mut reports, next_taker_id, &mut checksum);
        reports.clear();
    }
    let mut latencies = vec![0_u64; samples];
    let before = snapshot_gate();
    for sample in &mut latencies {
        let elapsed = step_ioc_full(&mut book, &mut reports, next_taker_id, &mut checksum);
        *sample = u64::try_from(elapsed).unwrap_or(u64::MAX);
        reports.clear();
    }
    let after = snapshot_gate();
    assert_gate(before, after, "tif ioc_full");
    finish(
        out,
        BenchRecord::new(
            "component",
            "book",
            "ioc_full",
            &[("tif", Extra::Text("ioc"))],
        ),
        checksum,
        &mut latencies,
    );
}

/// FOK rejection against a shallow ask; the fixture survives every attempt.
fn run_fok_reject(
    samples: usize,
    next_taker_id: &mut u64,
    replenish_id: &mut u64,
    out: &mut Vec<BenchRecord>,
) {
    let mut book = std::boxed::Box::new(BenchBook::new(INSTRUMENT));
    let mut reports = Reports::new();
    rest_fixture(&mut book, &mut reports, replenish_id, 100, 3);
    let mut checksum = 0_u64;
    for _ in 0..WARMUP_SAMPLES {
        step_fok_reject(&mut book, &mut reports, next_taker_id, &mut checksum);
    }
    let mut latencies = vec![0_u64; samples];
    let before = snapshot_gate();
    for sample in &mut latencies {
        let elapsed = step_fok_reject(&mut book, &mut reports, next_taker_id, &mut checksum);
        *sample = u64::try_from(elapsed).unwrap_or(u64::MAX);
    }
    let after = snapshot_gate();
    assert_gate(before, after, "tif fok_reject");
    finish(
        out,
        BenchRecord::new(
            "component",
            "book",
            "fok_reject",
            &[("tif", Extra::Text("fok"))],
        ),
        checksum,
        &mut latencies,
    );
}

/// FOK single-unit fills from a deep ask that never depletes.
fn run_fok_single(
    samples: usize,
    next_taker_id: &mut u64,
    replenish_id: &mut u64,
    out: &mut Vec<BenchRecord>,
) {
    let mut book = std::boxed::Box::new(BenchBook::new(INSTRUMENT));
    let mut reports = Reports::new();
    rest_fixture(&mut book, &mut reports, replenish_id, 100, 10_000_000);
    let mut checksum = 0_u64;
    for _ in 0..WARMUP_SAMPLES {
        step_fok_single(&mut book, &mut reports, next_taker_id, &mut checksum);
        reports.clear();
    }
    let mut latencies = vec![0_u64; samples];
    let before = snapshot_gate();
    for sample in &mut latencies {
        let elapsed = step_fok_single(&mut book, &mut reports, next_taker_id, &mut checksum);
        *sample = u64::try_from(elapsed).unwrap_or(u64::MAX);
        reports.clear();
    }
    let after = snapshot_gate();
    assert_gate(before, after, "tif fok_single");
    finish(
        out,
        BenchRecord::new(
            "component",
            "book",
            "fok_single",
            &[("tif", Extra::Text("fok"))],
        ),
        checksum,
        &mut latencies,
    );
}

/// FOK sweeps of an eight-level fan, re-rested from staged prices each sample.
fn run_fok_multi(
    samples: usize,
    next_taker_id: &mut u64,
    replenish_id: &mut u64,
    out: &mut Vec<BenchRecord>,
) {
    let mut book = std::boxed::Box::new(BenchBook::new(INSTRUMENT));
    let mut reports = Reports::new();
    for offset in 0..8_usize {
        let price = 100 + i64::try_from(offset).unwrap_or(i64::MAX);
        rest_fixture(&mut book, &mut reports, replenish_id, price, 1);
    }
    let mut checksum = 0_u64;
    for _ in 0..WARMUP_SAMPLES {
        step_fok_multi(&mut book, &mut reports, next_taker_id, &mut checksum);
        replenish_fan(&mut book, &mut reports, replenish_id);
    }
    let mut latencies = vec![0_u64; samples];
    let before = snapshot_gate();
    for sample in &mut latencies {
        let elapsed = step_fok_multi(&mut book, &mut reports, next_taker_id, &mut checksum);
        *sample = u64::try_from(elapsed).unwrap_or(u64::MAX);
        replenish_fan(&mut book, &mut reports, replenish_id);
    }
    let after = snapshot_gate();
    assert_gate(before, after, "tif fok_multi");
    finish(
        out,
        BenchRecord::new(
            "component",
            "book",
            "fok_multi",
            &[("tif", Extra::Text("fok"))],
        ),
        checksum,
        &mut latencies,
    );
}

/// GTC full fills, the direct comparison cell for `ioc_full`.
fn run_gtc_full(
    samples: usize,
    next_taker_id: &mut u64,
    replenish_id: &mut u64,
    out: &mut Vec<BenchRecord>,
) {
    let mut book = std::boxed::Box::new(BenchBook::new(INSTRUMENT));
    let mut reports = Reports::new();
    rest_fixture(&mut book, &mut reports, replenish_id, 100, 10_000_000);
    let mut checksum = 0_u64;
    for _ in 0..WARMUP_SAMPLES {
        step_gtc_full(&mut book, &mut reports, next_taker_id, &mut checksum);
        reports.clear();
    }
    let mut latencies = vec![0_u64; samples];
    let before = snapshot_gate();
    for sample in &mut latencies {
        let elapsed = step_gtc_full(&mut book, &mut reports, next_taker_id, &mut checksum);
        *sample = u64::try_from(elapsed).unwrap_or(u64::MAX);
        reports.clear();
    }
    let after = snapshot_gate();
    assert_gate(before, after, "tif gtc_full");
    finish(
        out,
        BenchRecord::new(
            "component",
            "book",
            "gtc_full",
            &[("tif", Extra::Text("gtc"))],
        ),
        checksum,
        &mut latencies,
    );
}

/// GTC partial fills that rest two units; the bid is cancelled untimed and
/// the maker replenished before the next sample.
fn run_gtc_partial_rests(
    samples: usize,
    next_taker_id: &mut u64,
    replenish_id: &mut u64,
    out: &mut Vec<BenchRecord>,
) {
    let mut book = std::boxed::Box::new(BenchBook::new(INSTRUMENT));
    let mut reports = Reports::new();
    rest_fixture(&mut book, &mut reports, replenish_id, 100, 3);
    let mut checksum = 0_u64;
    for _ in 0..WARMUP_SAMPLES {
        let (_, rested_bid) =
            step_gtc_partial_rests(&mut book, &mut reports, next_taker_id, &mut checksum);
        teardown_gtc_partial(&mut book, &mut reports, rested_bid, replenish_id);
    }
    let mut latencies = vec![0_u64; samples];
    let before = snapshot_gate();
    for sample in &mut latencies {
        let (elapsed, rested_bid) =
            step_gtc_partial_rests(&mut book, &mut reports, next_taker_id, &mut checksum);
        *sample = u64::try_from(elapsed).unwrap_or(u64::MAX);
        teardown_gtc_partial(&mut book, &mut reports, rested_bid, replenish_id);
    }
    let after = snapshot_gate();
    assert_gate(before, after, "tif gtc_partial_rests");
    finish(
        out,
        BenchRecord::new(
            "component",
            "book",
            "gtc_partial_rests",
            &[("tif", Extra::Text("gtc"))],
        ),
        checksum,
        &mut latencies,
    );
}
