#![forbid(unsafe_code)]

use hft_gateway::Gateway;
use hft_replay::replay;
use hft_risk::{RiskEngine, RiskLimits};
use hft_types::{
    AccountId, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, SequenceNumber, Side,
};
use hft_wire::encode_new_order;

fn main() {
    if let Err(error) = run() {
        eprintln!("hft-cli: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), &'static str> {
    let Some(command) = std::env::args().nth(1) else {
        return Err("usage: hft-cli replay-demo");
    };
    if command != "replay-demo" {
        return Err("unknown command; expected replay-demo");
    }
    let limits = RiskLimits {
        max_quantity: Quantity(100),
        max_notional: 100_000,
        max_abs_position: Quantity(1_000),
        max_open_orders: 8,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    };
    let mut risk = RiskEngine::<2, 8>::new();
    risk.register_account(AccountId(1), limits)
        .map_err(|_| "failed to register account 1")?;
    risk.register_account(AccountId(2), limits)
        .map_err(|_| "failed to register account 2")?;
    let mut gateway = Gateway::<2, 8, 4, 4>::new(risk, InstrumentId(1));
    let make = |id, account, side| {
        encode_new_order(NewOrder {
            order_id: OrderId(id),
            account_id: AccountId(account),
            instrument_id: InstrumentId(1),
            price: PriceTicks(100),
            quantity: Quantity(5),
            sequence: SequenceNumber(id),
            side,
        })
    };
    let sell = make(1, 1, Side::Sell);
    let buy = make(2, 2, Side::Buy);
    let summary =
        replay::<2, 8, 4, 4, 4>(&mut gateway, &[&sell, &buy]).map_err(|_| "replay failed")?;
    println!(
        "frames={} reports={} digest={:016x}",
        summary.frames, summary.reports, summary.digest
    );
    Ok(())
}
