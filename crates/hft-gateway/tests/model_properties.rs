//! Seeded sessions drive the real gateway and the reference model over the
//! same frames. Every step checks outcome equivalence and account snapshots;
//! the model invariant encodes reservation-equals-live-exposure. Replaying
//! collected frames through independent gateways reproduces the digest.

use std::collections::HashSet;

use hft_gateway::{Gateway, GatewayError, GatewayOutcome};
use hft_io::RxFrame;
use hft_model::{
    Command, CommandGen, GenConfig, ModelEngine, ModelLimits, ModelNewOutcome, ModelRejection,
};
use hft_risk::{RiskEngine, RiskLimits};
use hft_types::{AccountId, InstrumentId, OrderState, PriceTicks, Quantity, ReportBuffer};
use hft_wire::{encode_cancel_order, encode_new_order};

const ACCOUNT_IDS: [AccountId; 2] = [AccountId(1), AccountId(2)];
const RISK_ORDERS: usize = 32;
const LEVELS: usize = 8;
const ORDERS_PER_LEVEL: usize = 8;
const REPORTS: usize = 8;

const SEEDS: [u64; 4] = [0x0a7c_0001, 0x0a7c_0002, 0x0a7c_0003, 0x0a7c_0004];
const STEPS: u64 = 300;

fn engine_limits() -> RiskLimits {
    RiskLimits {
        max_quantity: Quantity(3),
        max_notional: 100_000,
        max_abs_position: Quantity(12),
        max_open_orders: 6,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    }
}

fn model_limits() -> ModelLimits {
    let limits = engine_limits();
    ModelLimits {
        max_quantity: limits.max_quantity.0,
        max_notional: limits.max_notional,
        max_abs_position: i128::from(limits.max_abs_position.0),
        max_open_orders: limits.max_open_orders,
        minimum_price: limits.minimum_price.0,
        maximum_price: limits.maximum_price.0,
    }
}

fn gen_config() -> GenConfig {
    GenConfig {
        accounts: 2,
        minimum_price: 98,
        maximum_price: 102,
        // Above the risk cap so quantity rejections occur deterministically.
        max_quantity: 4,
        cancel_probability_pct: 40,
        duplicate_id_probability_pct: 5,
        ioc_probability_pct: 15,
        fok_probability_pct: 10,
    }
}

fn fresh_gateway() -> Gateway<2, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL> {
    let mut risk = RiskEngine::<2, RISK_ORDERS>::new();
    for account in ACCOUNT_IDS {
        risk.register_account(account, engine_limits())
            .expect("account registration");
    }
    Gateway::new(risk, InstrumentId(1))
}

fn map_rejection(error: &GatewayError) -> ModelRejection {
    match error {
        GatewayError::Risk(reason) => ModelRejection::Risk(*reason),
        GatewayError::Book(reason) => ModelRejection::Book(*reason),
        GatewayError::Parse(_) | GatewayError::Sequence { .. } => {
            panic!("generator produced an invalid frame: {error:?}")
        }
        GatewayError::RiskState(reason) => panic!("risk state diverged: {reason:?}"),
    }
}

fn assert_snapshots_match(
    gateway: &Gateway<2, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>,
    model: &ModelEngine,
    step: u64,
) {
    for account in ACCOUNT_IDS {
        assert_eq!(
            gateway.risk().account_snapshot(account),
            model.account_view(account),
            "account snapshot at step {step}"
        );
    }
}

#[test]
fn seeded_sessions_match_the_reference_model_and_replay() {
    for seed in SEEDS {
        run_session(seed);
    }
}

fn run_session(seed: u64) {
    let mut gateway = fresh_gateway();
    let mut model = ModelEngine::new(InstrumentId(1), LEVELS, ORDERS_PER_LEVEL);
    for account in ACCOUNT_IDS {
        model.register_account(account, model_limits());
    }
    let mut reports = ReportBuffer::<REPORTS>::new();
    let mut generator = CommandGen::new(gen_config(), InstrumentId(1), seed);
    let mut frames: std::vec::Vec<std::vec::Vec<u8>> = std::vec::Vec::new();
    // Ids that reached a terminal state (fully filled or canceled). Each must
    // leave the book exactly once: a later successful cancel for such an id
    // would be a double terminal transition.
    let mut terminal_ids: HashSet<u64> = HashSet::new();
    let mut summarized_fills = 0_u64;
    let mut reported_fills = 0_u64;

    for step in 0..STEPS {
        match generator.next_command() {
            Command::New(order) => {
                assert_eq!(order.sequence.0, step + 1, "sequence drift");
                let expected = model.apply_new(&order, REPORTS);
                if let Ok(outcome) = &expected {
                    summarized_fills += outcome.filled.0;
                    if outcome.rested.0 == 0 && !outcome.fills.is_empty() {
                        terminal_ids.insert(order.order_id.0);
                    }
                }
                let bytes = encode_new_order(order);
                let actual = gateway.process_frame(&RxFrame::from_bytes(&bytes), &mut reports);
                reported_fills += compare_new(&order, actual, &expected, &reports);
                frames.push(bytes.to_vec());
            }
            Command::Cancel(cancel) => {
                assert_eq!(cancel.sequence.0, step + 1, "sequence drift");
                let expected = model.apply_cancel(&cancel);
                let bytes = encode_cancel_order(cancel);
                let actual = gateway.process_frame(&RxFrame::from_bytes(&bytes), &mut reports);
                compare_cancel(actual, expected, &mut terminal_ids);
                frames.push(bytes.to_vec());
            }
        }

        // Reservation equals live exposure inside the mirror; snapshot
        // equivalence transfers the property to the real engine.
        model.assert_consistent();
        assert_snapshots_match(&gateway, &model, step);
    }

    assert_eq!(
        reported_fills, summarized_fills,
        "reported fill quantity must equal summary totals"
    );
    assert_replay_equality(&gateway, &frames);
}

/// Compares one new-order outcome pair and returns the reported fill quantity.
fn compare_new(
    order: &hft_types::NewOrder,
    actual: Result<GatewayOutcome, GatewayError>,
    expected: &Result<ModelNewOutcome, ModelRejection>,
    reports: &ReportBuffer<REPORTS>,
) -> u64 {
    match (&actual, &expected) {
        (Ok(GatewayOutcome::NewOrder(summary)), Ok(outcome)) => {
            assert_eq!(summary.filled_quantity, outcome.filled, "filled");
            assert_eq!(summary.resting_quantity, outcome.rested, "rested");
            let expected_state = if outcome.filled.0 == order.quantity.0 {
                OrderState::Filled
            } else if outcome.filled.0 == 0 {
                OrderState::Accepted
            } else {
                OrderState::PartiallyFilled
            };
            assert_eq!(summary.state, expected_state, "state");
            let actual_fills: std::vec::Vec<_> = reports
                .iter()
                .map(|report| (report.maker_order_id, report.price, report.quantity))
                .collect();
            let expected_fills: std::vec::Vec<_> = outcome
                .fills
                .iter()
                .map(|fill| (fill.maker_order_id, fill.price, fill.quantity))
                .collect();
            // Identical fill sequences prove price-time priority.
            assert_eq!(actual_fills, expected_fills, "fills");
            let mut total = 0_u64;
            for report in reports.iter() {
                assert_eq!(report.taker_order_id, order.order_id);
                total += report.quantity.0;
            }
            total
        }
        (Err(error), Err(rejection)) => {
            assert_eq!(map_rejection(error), *rejection, "rejection");
            0
        }
        (actual, expected) => panic!("new-order divergence: {actual:?} vs {expected:?}"),
    }
}

fn compare_cancel(
    actual: Result<GatewayOutcome, GatewayError>,
    expected: Result<hft_model::ModelCancelled, ModelRejection>,
    terminal_ids: &mut HashSet<u64>,
) {
    match (&actual, &expected) {
        (Ok(GatewayOutcome::Cancelled(cancelled)), Ok(model_cancelled)) => {
            assert!(
                !terminal_ids.contains(&cancelled.order_id.0),
                "double terminal transition for {}",
                cancelled.order_id.0
            );
            terminal_ids.insert(cancelled.order_id.0);
            assert_eq!(cancelled.order_id, model_cancelled.order_id);
            assert_eq!(cancelled.account_id, model_cancelled.account_id);
            assert_eq!(cancelled.quantity, model_cancelled.quantity);
        }
        (Err(error), Err(rejection)) => {
            assert_eq!(map_rejection(error), *rejection, "cancel rejection");
        }
        (actual, expected) => panic!("cancel divergence: {actual:?} vs {expected:?}"),
    }
}

fn assert_replay_equality(
    gateway: &Gateway<2, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>,
    frames: &[std::vec::Vec<u8>],
) {
    let borrowed: std::vec::Vec<&[u8]> = frames.iter().map(std::vec::Vec::as_slice).collect();
    let mut first_replay = fresh_gateway();
    let mut second_replay = fresh_gateway();
    for replay_gateway in [&mut first_replay, &mut second_replay] {
        for frame in &borrowed {
            let mut reports = ReportBuffer::<REPORTS>::new();
            // Business rejections are part of the stream and must land
            // identically; only their equality matters here.
            let _ = replay_gateway.process_frame(&RxFrame::from_bytes(frame), &mut reports);
        }
    }
    assert_eq!(
        first_replay.stable_digest(),
        gateway.stable_digest(),
        "first replay digest"
    );
    assert_eq!(
        second_replay.stable_digest(),
        gateway.stable_digest(),
        "second replay digest"
    );
}

/// Model-prescreened accepted-only stream: `hft_replay` must reproduce the
/// direct-processing digest when no frame is rejected.
#[test]
fn accepted_frames_replay_through_the_replay_helper() {
    let mut generous_engine_limits = engine_limits();
    generous_engine_limits.max_quantity = Quantity(8);
    generous_engine_limits.max_abs_position = Quantity(1_000);
    generous_engine_limits.max_open_orders = 64;
    let mut generous_model_limits = model_limits();
    generous_model_limits.max_quantity = 8;
    generous_model_limits.max_abs_position = 1_000;
    generous_model_limits.max_open_orders = 64;

    let config = GenConfig {
        accounts: 2,
        minimum_price: 98,
        maximum_price: 102,
        max_quantity: 4,
        cancel_probability_pct: 0,
        duplicate_id_probability_pct: 0,
        ioc_probability_pct: 0,
        fok_probability_pct: 0,
    };
    let mut generator = CommandGen::new(config, InstrumentId(1), 0x0a7c_0005);
    let mut oracle = ModelEngine::new(InstrumentId(1), LEVELS, ORDERS_PER_LEVEL);
    for account in ACCOUNT_IDS {
        oracle.register_account(account, generous_model_limits);
    }
    let mut frames: std::vec::Vec<std::vec::Vec<u8>> = std::vec::Vec::new();
    for _ in 0..40 {
        if let Command::New(order) = generator.next_command() {
            if oracle.apply_new(&order, REPORTS).is_ok() {
                frames.push(encode_new_order(order).to_vec());
            }
        }
    }
    assert!(
        u64::try_from(frames.len()).is_ok_and(|count| count >= 20),
        "prescreen kept {}",
        frames.len()
    );

    let mut risk = RiskEngine::<2, RISK_ORDERS>::new();
    for account in ACCOUNT_IDS {
        risk.register_account(account, generous_engine_limits)
            .expect("account");
    }
    let mut gateway = Gateway::new(risk, InstrumentId(1));
    let borrowed: std::vec::Vec<&[u8]> = frames.iter().map(std::vec::Vec::as_slice).collect();
    let summary = hft_replay::replay::<2, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL, REPORTS>(
        &mut gateway,
        &borrowed,
    )
    .expect("accepted stream replays cleanly");
    assert_eq!(summary.digest, gateway.stable_digest());
}
