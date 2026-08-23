//! Seeded array-model comparisons across book shapes, without private-state
//! dumps: outcomes and fill sequences must match the reference model.

use hft_book::OrderBook;
use hft_model::{Command, CommandGen, GenConfig, ModelBook};
use hft_types::{InstrumentId, ReportBuffer};

fn run_shape<const LEVELS: usize, const ORDERS: usize, const REPORTS: usize>(
    seed: u64,
    steps: u64,
) {
    let mut book = OrderBook::<LEVELS, ORDERS>::new(InstrumentId(1));
    let mut model = ModelBook::default();
    let mut reports = ReportBuffer::<REPORTS>::new();
    let mut generator = CommandGen::new(
        GenConfig {
            accounts: 2,
            minimum_price: 98,
            maximum_price: 102,
            max_quantity: 4,
            cancel_probability_pct: 55,
            duplicate_id_probability_pct: 8,
            ioc_probability_pct: 15,
            fok_probability_pct: 10,
        },
        InstrumentId(1),
        seed,
    );
    for step in 0..steps {
        match generator.next_command() {
            Command::New(command) => {
                reports.clear();
                let actual = book.submit(command, &mut reports);
                let expected = model.submit(&command, REPORTS, LEVELS, ORDERS);
                match (actual, expected) {
                    (Ok(summary), Ok((state, filled, resting, discarded, fills))) => {
                        assert_eq!(summary.state, state, "state at step {step}");
                        assert_eq!(summary.filled_quantity, filled);
                        assert_eq!(summary.resting_quantity, resting);
                        assert_eq!(summary.discarded_quantity, discarded);
                        // Quantity conservation and a non-negative remainder.
                        assert_eq!(
                            summary.filled_quantity.0
                                + summary.resting_quantity.0
                                + summary.discarded_quantity.0,
                            command.quantity.0,
                            "conservation at step {step}"
                        );
                        let actual_fills: std::vec::Vec<_> =
                            reports.iter().map(|report| report.maker_order_id).collect();
                        let expected_fills: std::vec::Vec<_> =
                            fills.iter().map(|fill| fill.maker_order_id).collect();
                        // Identical fill order is the price-time priority proof.
                        assert_eq!(actual_fills, expected_fills, "priority at step {step}");
                    }
                    (Err(actual_error), Err(expected_error)) => {
                        assert_eq!(actual_error, expected_error, "rejection at step {step}");
                    }
                    (actual, expected) => {
                        panic!("submit divergence at step {step}: {actual:?} vs {expected:?}");
                    }
                }
            }
            Command::Cancel(command) => {
                let actual = book.cancel(command);
                let expected = model.cancel(&command);
                match (actual, expected) {
                    (Ok(cancelled), Ok((id, _, quantity))) => {
                        assert_eq!(cancelled.order_id.0, id);
                        assert_eq!(
                            cancelled.quantity.0, quantity,
                            "cancel quantity at step {step}"
                        );
                    }
                    (Err(actual_error), Err(expected_error)) => {
                        assert_eq!(
                            actual_error, expected_error,
                            "cancel rejection at step {step}"
                        );
                    }
                    (actual, expected) => {
                        panic!("cancel divergence at step {step}: {actual:?} vs {expected:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn seeded_sessions_match_reference_model_across_shapes() {
    run_shape::<4, 4, 8>(0x1111_1111_1111_1111, 800);
    run_shape::<2, 2, 2>(0x2222_2222_2222_2222, 600);
    run_shape::<8, 2, 16>(0x3333_3333_3333_3333, 800);
    run_shape::<3, 5, 6>(0x4444_4444_4444_4444, 600);
}
