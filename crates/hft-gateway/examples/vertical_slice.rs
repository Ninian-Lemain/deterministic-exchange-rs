use hft_gateway::Gateway;
use hft_io::RxFrame;
use hft_risk::{RiskEngine, RiskLimits};
use hft_types::{
    AccountId, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, ReportBuffer, SequenceNumber,
    Side,
};
use hft_wire::encode_new_order;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let limits = RiskLimits {
        max_quantity: Quantity(10),
        max_notional: 10_000,
        max_abs_position: Quantity(100),
        max_open_orders: 4,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    };
    let mut risk = RiskEngine::<2, 4>::new();
    risk.register_account(AccountId(1), limits)
        .map_err(|error| format!("account 1: {error:?}"))?;
    risk.register_account(AccountId(2), limits)
        .map_err(|error| format!("account 2: {error:?}"))?;
    let mut gateway = Gateway::<2, 4, 2, 2>::new(risk, InstrumentId(7));
    let mut reports = ReportBuffer::<2>::new();
    for (id, account, side) in [(1, 1, Side::Sell), (2, 2, Side::Buy)] {
        let bytes = encode_new_order(NewOrder {
            time_in_force: hft_types::TimeInForce::Gtc,
            order_id: OrderId(id),
            account_id: AccountId(account),
            instrument_id: InstrumentId(7),
            price: PriceTicks(100),
            quantity: Quantity(5),
            sequence: SequenceNumber(id),
            side,
        });
        gateway
            .process_frame(&RxFrame::from_bytes(&bytes), &mut reports)
            .map_err(|error| format!("gateway: {error:?}"))?;
    }
    for report in reports.iter() {
        println!("{report:?}");
    }
    Ok(())
}
