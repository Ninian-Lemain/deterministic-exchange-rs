//! Timer-resolution-resistant measurements. Inputs and fixtures are prepared
//! outside each interval; latency is nanoseconds per operation in a batch.

use crate::record::{BenchRecord, Extra};
use crate::{SuiteConfig, allocation_gate, analyze, assert_allocation_gate};
use hft_book::OrderBook;
use hft_gateway::{Gateway, GatewayError, GatewayOutcome};
use hft_io::RxFrame;
use hft_risk::{RiskEngine, RiskLimits};
use hft_types::{
    AccountId, CancelOrder, Command, InstrumentId, MatchSummary, NewOrder, OrderId, OrderState,
    PriceTicks, Quantity, ReportBuffer, SequenceNumber, Side, TimeInForce,
};
use hft_wire::encode_new_order;
use std::{hint::black_box, time::Instant};

const BATCH: usize = 64;
const BATCH_U64: u64 = 64;
const WARMUP_BATCHES: usize = 8;
const MAX_SAMPLES: usize = 2_000;
type PairGateway = Gateway<2, 8, 2, 2>;
type Risk = RiskEngine<64, 128>;
type Book = OrderBook<2, BATCH>;

struct Measurements {
    timings: [u64; MAX_SAMPLES],
    count: usize,
    elapsed_ns: u128,
    checksum: u64,
}

impl Measurements {
    fn push(
        mut self,
        out: &mut Vec<BenchRecord>,
        boundary: &'static str,
        component: &'static str,
        scenario: &'static str,
        path: &'static str,
    ) {
        if self.count == 0 {
            return;
        }
        let operations = u64::try_from(self.count).expect("sample count") * BATCH_U64;
        let stats = analyze(&mut self.timings[..self.count]);
        out.push(BenchRecord {
            samples: self.count,
            mean_ns: narrow(self.elapsed_ns / u128::from(operations)),
            p50_ns: stats.p50,
            p90_ns: stats.p90,
            p99_ns: stats.p99,
            p99_9_ns: stats.p99_9,
            max_ns: stats.max,
            // Use the raw accumulated time, avoiding reciprocal error from
            // integer-rounded per-operation samples (especially for lookups).
            ops_per_second: narrow(
                (u128::from(operations) * 1_000_000_000)
                    .checked_div(self.elapsed_ns)
                    .unwrap_or(0),
            ),
            checksum: self.checksum,
            ..BenchRecord::new(
                boundary,
                component,
                scenario,
                &[
                    ("batch", Extra::U64(BATCH_U64)),
                    ("commands", Extra::U64(operations)),
                    ("path", Extra::Text(path)),
                ],
            )
        });
    }
}

fn narrow(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn measure<Fixture, Observation>(
    samples: usize,
    mut prepare: impl FnMut() -> Fixture,
    mut run: impl FnMut(&mut Fixture) -> Observation,
    mut inspect: impl FnMut(&Fixture, &Observation) -> u64,
) -> Measurements {
    let mut result = Measurements {
        timings: [0; MAX_SAMPLES],
        count: samples.min(MAX_SAMPLES),
        elapsed_ns: 0,
        checksum: 0,
    };
    if result.count == 0 {
        return result;
    }
    let gate = allocation_gate();
    for index in 0..WARMUP_BATCHES + result.count {
        let mut fixture = prepare();
        let started = Instant::now();
        let observation = run(black_box(&mut fixture));
        black_box(&observation);
        let elapsed = started.elapsed().as_nanos();
        // Validation, state digests, and fixture reset are never timed.
        let contribution = inspect(black_box(&fixture), &observation);
        if index >= WARMUP_BATCHES {
            result.timings[index - WARMUP_BATCHES] = narrow(elapsed / u128::from(BATCH_U64));
            result.elapsed_ns += elapsed;
            result.checksum = result.checksum.rotate_left(7).wrapping_add(contribution);
        }
    }
    assert_allocation_gate(gate, "batched workload");
    result
}

fn batch<T>(operation: impl FnMut(usize) -> T) -> [T; BATCH] {
    std::array::from_fn(operation)
}

/// Adds precision cells alongside the original suite, using its existing
/// per-component sample budgets. A gateway operation is one message, so a
/// timed gateway batch contains 32 complete rest/fill pairs.
///
/// # Panics
///
/// Panics on an unexpected operation result, final state, or allocation.
pub fn batched_benchmarks(config: SuiteConfig, out: &mut Vec<BenchRecord>) {
    gateway_pairs(config.gateway_samples, out);
    risk_operations(config.risk_samples, out);
    book_submissions(config.plan_samples, out);
    book_index_operations(config.fifo_samples, out);
}

fn limits() -> RiskLimits {
    RiskLimits {
        max_quantity: Quantity(128),
        max_notional: 100_000,
        max_abs_position: Quantity(128),
        max_open_orders: 128,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    }
}

fn order(index: usize, account_id: AccountId, side: Side) -> NewOrder {
    let id = u64::try_from(index).expect("batch index") + 1;
    NewOrder {
        order_id: OrderId(id),
        account_id,
        instrument_id: InstrumentId(1),
        price: PriceTicks(100),
        quantity: Quantity(1),
        sequence: SequenceNumber(id),
        side,
        time_in_force: TimeInForce::Gtc,
    }
}

fn pair_fixture() -> (PairGateway, ReportBuffer<1>) {
    let mut risk = RiskEngine::new();
    for id in [AccountId(1), AccountId(2)] {
        risk.register_account(id, limits()).expect("pair account");
    }
    (Gateway::new(risk, InstrumentId(1)), ReportBuffer::new())
}

fn gateway_pairs(samples: usize, out: &mut Vec<BenchRecord>) {
    let orders = batch(|index| {
        if index % 2 == 0 {
            order(index, AccountId(1), Side::Sell)
        } else {
            order(index, AccountId(2), Side::Buy)
        }
    });
    let bytes = orders.map(encode_new_order);
    let frames = batch(|index| RxFrame::from_bytes(&bytes[index]));
    let commands = orders.map(Command::NewOrder);
    let frames_result = measure(
        samples,
        pair_fixture,
        |(gateway, reports)| {
            batch(|index| {
                black_box(&mut *gateway).process_frame(black_box(&frames[index]), reports)
            })
        },
        inspect_pairs,
    );
    let commands_result = measure(
        samples,
        pair_fixture,
        |(gateway, reports)| {
            batch(|index| {
                black_box(&mut *gateway).process_command(black_box(commands[index]), reports)
            })
        },
        inspect_pairs,
    );
    assert_eq!(frames_result.checksum, commands_result.checksum);
    frames_result.push(out, "gateway", "gateway", "pair_rest_fill_batched", "frame");
    commands_result.push(
        out,
        "gateway",
        "gateway",
        "pair_rest_fill_batched",
        "command",
    );
}

fn inspect_pairs(
    (gateway, reports): &(PairGateway, ReportBuffer<1>),
    results: &[Result<GatewayOutcome, GatewayError>; BATCH],
) -> u64 {
    let mut checksum = 0_u64;
    for (index, result) in results.iter().copied().enumerate() {
        let GatewayOutcome::NewOrder(summary) = result.expect("batched gateway order") else {
            unreachable!("fixture contains only new orders");
        };
        checksum = checksum.wrapping_add(inspect_summary(summary, index % 2 != 0));
    }
    assert_eq!(gateway.expected_sequence(), SequenceNumber(BATCH_U64 + 1));
    assert_eq!(
        gateway.risk().account_snapshot(AccountId(1)),
        Some((-32, 0))
    );
    assert_eq!(gateway.risk().account_snapshot(AccountId(2)), Some((32, 0)));
    let report = reports.iter().next().expect("last gateway fill");
    assert_eq!(report.maker_order_id, OrderId(BATCH_U64 - 1));
    assert_eq!(report.taker_order_id, OrderId(BATCH_U64));
    assert_eq!(report.quantity, Quantity(1));
    checksum.wrapping_add(gateway.stable_digest())
}

fn inspect_summary(summary: MatchSummary, filled: bool) -> u64 {
    assert_eq!(summary.filled_quantity, Quantity(u64::from(filled)));
    assert_eq!(summary.resting_quantity, Quantity(u64::from(!filled)));
    assert_eq!(summary.discarded_quantity, Quantity(0));
    assert_eq!(summary.report_count, usize::from(filled));
    assert_eq!(
        summary.state,
        if filled {
            OrderState::Filled
        } else {
            OrderState::Accepted
        }
    );
    summary.filled_quantity.0 * 3 + summary.resting_quantity.0
}

fn risk_fixture(orders: &[NewOrder]) -> Risk {
    let mut risk = Risk::new();
    for index in 1..=BATCH {
        let account_id = AccountId(u32::try_from(index).expect("account index"));
        risk.register_account(account_id, limits())
            .expect("risk account");
    }
    for &order in orders {
        risk.check_and_reserve(order)
            .expect("risk fixture reservation");
    }
    risk
}

fn inspect_risk(risk: &Risk, open_orders: u32) -> u64 {
    for index in 1..=BATCH {
        let account_id = AccountId(u32::try_from(index).expect("account index"));
        assert_eq!(risk.account_snapshot(account_id), Some((1, open_orders)));
    }
    risk.stable_digest()
}

fn risk_operations(samples: usize, out: &mut Vec<BenchRecord>) {
    let orders = batch(|index| {
        order(
            index,
            AccountId(u32::try_from(index + 1).expect("account index")),
            Side::Buy,
        )
    });
    measure(
        samples,
        || risk_fixture(&[]),
        |risk| batch(|index| black_box(&mut *risk).check_and_reserve(black_box(orders[index]))),
        |risk, results| {
            for result in results.iter().copied() {
                result.expect("batched reserve");
            }
            inspect_risk(risk, 1)
        },
    )
    .push(
        out,
        "component",
        "risk",
        "risk_check_batched",
        "check_and_reserve",
    );
    measure(
        samples,
        || risk_fixture(&orders),
        |risk| {
            batch(|index| {
                black_box(&mut *risk).record_fill(black_box(orders[index].order_id), Quantity(1))
            })
        },
        |risk, results| {
            for result in results.iter().copied() {
                result.expect("batched fill");
            }
            inspect_risk(risk, 0)
        },
    )
    .push(out, "component", "risk", "fill_batched", "record_fill");
    measure(
        samples,
        || risk_fixture(&orders),
        |risk| {
            batch(|index| black_box(&*risk).account_snapshot(black_box(orders[index].account_id)))
        },
        |risk, results| {
            let mut checksum = 0_u64;
            for result in results.iter().copied() {
                assert_eq!(result, Some((1, 1)));
                let (position, open) = result.expect("batched account lookup");
                checksum = checksum.wrapping_add(
                    u64::try_from(position).expect("positive fixture position") + u64::from(open),
                );
            }
            checksum.wrapping_add(inspect_risk(risk, 1))
        },
    )
    .push(
        out,
        "component",
        "risk",
        "account_lookup_batched",
        "account_snapshot",
    );
    measure(
        samples,
        || risk_fixture(&orders),
        |risk| {
            batch(|index| {
                black_box(&*risk).can_cancel(
                    black_box(orders[index].order_id),
                    black_box(orders[index].account_id),
                )
            })
        },
        |risk, results| {
            for result in results.iter().copied() {
                result.expect("batched reservation lookup");
            }
            inspect_risk(risk, 1)
        },
    )
    .push(
        out,
        "component",
        "risk",
        "reservation_lookup_batched",
        "can_cancel",
    );
}

fn book_fixture(orders: &[NewOrder]) -> (Book, ReportBuffer<1>) {
    let mut book = Book::new(InstrumentId(1));
    let mut reports = ReportBuffer::new();
    for &order in orders {
        book.submit(order, &mut reports).expect("rest book fixture");
    }
    (book, reports)
}

fn book_submissions(samples: usize, out: &mut Vec<BenchRecord>) {
    let maker = NewOrder {
        quantity: Quantity(BATCH_U64),
        ..order(0, AccountId(1), Side::Sell)
    };
    for filled in [false, true] {
        let orders = batch(|index| NewOrder {
            price: PriceTicks(if filled { 100 } else { 99 }),
            ..order(index + 1, AccountId(2), Side::Buy)
        });
        measure(
            samples,
            || {
                let (book, _) = book_fixture(&[maker]);
                // Independent buffers keep reset work outside the interval
                // and retain every operation's reports for validation.
                (book, batch(|_| ReportBuffer::<1>::new()))
            },
            |(book, reports)| {
                batch(|index| {
                    black_box(&mut *book).submit(black_box(orders[index]), &mut reports[index])
                })
            },
            |(book, reports), results| {
                let mut checksum = 0_u64;
                for (index, result) in results.iter().copied().enumerate() {
                    checksum = checksum
                        .wrapping_add(inspect_summary(result.expect("batched book order"), filled));
                    assert_eq!(reports[index].len(), usize::from(filled));
                    if filled {
                        let report = reports[index].iter().next().expect("single fill report");
                        assert_eq!(report.maker_order_id, maker.order_id);
                        assert_eq!(report.taker_order_id, orders[index].order_id);
                        assert_eq!(report.price, maker.price);
                        assert_eq!(report.quantity, Quantity(1));
                        checksum = checksum.wrapping_add(report.taker_order_id.0);
                    }
                }
                assert_eq!(book.order_count(), if filled { 0 } else { BATCH + 1 });
                checksum.wrapping_add(book.stable_digest())
            },
        )
        .push(
            out,
            "component",
            "book",
            if filled {
                "single_fill_batched"
            } else {
                "non_crossing_batched"
            },
            "submit",
        );
    }
}

fn book_index_operations(samples: usize, out: &mut Vec<BenchRecord>) {
    let orders = batch(|index| order(index, AccountId(1), Side::Sell));
    let cancels = orders.map(|order| CancelOrder {
        order_id: order.order_id,
        account_id: order.account_id,
        instrument_id: order.instrument_id,
        sequence: order.sequence,
    });
    measure(
        samples,
        || book_fixture(&orders),
        |(book, _)| batch(|index| black_box(&mut *book).cancel(black_box(cancels[index]))),
        |(book, _), results| {
            let mut checksum = 0_u64;
            for (index, result) in results.iter().copied().enumerate() {
                let cancelled = result.expect("batched cancel");
                assert_eq!(cancelled.order_id, orders[index].order_id);
                assert_eq!(cancelled.quantity, Quantity(1));
                checksum = checksum.wrapping_add(cancelled.order_id.0);
            }
            assert_eq!(book.order_count(), 0);
            checksum.wrapping_add(book.stable_digest())
        },
    )
    .push(out, "component", "book", "cancel_batched", "cancel");
    measure(
        samples,
        || book_fixture(&orders),
        |(book, _)| {
            batch(|index| black_box(&*book).contains_order(black_box(orders[index].order_id)))
        },
        |(book, _), results| {
            let mut checksum = 0_u64;
            for result in results.iter().copied() {
                assert!(result, "batched lookup targets a live order");
                checksum += u64::from(result);
            }
            assert_eq!(book.order_count(), BATCH);
            checksum.wrapping_add(book.stable_digest())
        },
    )
    .push(
        out,
        "component",
        "book",
        "order_lookup_batched",
        "contains_order",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throughput_uses_total_batch_time_before_rounding() {
        let mut timings = [0; MAX_SAMPLES];
        timings[0] = 1;
        timings[1] = 1;
        let measurement = Measurements {
            timings,
            count: 2,
            elapsed_ns: 250,
            checksum: 42,
        };
        let mut records = Vec::new();
        measurement.push(&mut records, "component", "risk", "lookup", "lookup");
        let record = &records[0];
        assert_eq!(record.samples, 2);
        assert_eq!(record.mean_ns, 1);
        assert_eq!(record.p50_ns, 1);
        assert_eq!(record.ops_per_second, 512_000_000);
        assert_eq!(record.checksum, 42);
        assert_eq!(record.params[1], ("commands", Extra::U64(128)));
    }
}
