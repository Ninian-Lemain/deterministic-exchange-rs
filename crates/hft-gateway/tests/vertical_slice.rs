use hft_gateway::Gateway;
use hft_io::RxFrame;
use hft_risk::{RiskEngine, RiskLimits};
use hft_types::{
    AccountId, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, ReportBuffer, SequenceNumber,
    Side,
};
use hft_wire::encode_new_order;

#[test]
fn fixture_parameters_drive_end_to_end_match() {
    let fixture = include_str!("../../../tests/fixtures/crossing_orders.txt");
    let mut lines = fixture.lines().filter(|line| !line.starts_with('#'));
    let sell = parse_order(lines.next().expect("sell fixture"));
    let buy = parse_order(lines.next().expect("buy fixture"));

    let limits = RiskLimits {
        max_quantity: Quantity(100),
        max_notional: 100_000,
        max_abs_position: Quantity(100),
        max_open_orders: 4,
        minimum_price: PriceTicks(1),
        maximum_price: PriceTicks(1_000),
    };
    let mut risk = RiskEngine::<2, 4>::new();
    risk.register_account(AccountId(1), limits)
        .expect("maker account");
    risk.register_account(AccountId(2), limits)
        .expect("taker account");
    let mut gateway = Gateway::<2, 4, 2, 2>::new(risk, InstrumentId(7));
    let mut reports = ReportBuffer::<2>::new();

    for order in [sell, buy] {
        let bytes = encode_new_order(order);
        gateway
            .process_frame(&RxFrame::from_bytes(&bytes), &mut reports)
            .expect("fixture order");
    }
    let report = reports.iter().next().expect("execution report");
    assert_eq!(report.maker_order_id, OrderId(1));
    assert_eq!(report.taker_order_id, OrderId(2));
    assert_eq!(report.quantity, Quantity(5));
}

fn parse_order(line: &str) -> NewOrder {
    let fields: Vec<_> = line.split(',').collect();
    assert_eq!(fields.len(), 7);
    NewOrder {
        time_in_force: hft_types::TimeInForce::Gtc,
        order_id: OrderId(fields[0].parse().expect("order ID")),
        account_id: AccountId(fields[1].parse().expect("account ID")),
        instrument_id: InstrumentId(fields[2].parse().expect("instrument ID")),
        price: PriceTicks(fields[3].parse().expect("price")),
        quantity: Quantity(fields[4].parse().expect("quantity")),
        sequence: SequenceNumber(fields[5].parse().expect("sequence")),
        side: match fields[6] {
            "B" => Side::Buy,
            "S" => Side::Sell,
            _ => panic!("invalid fixture side"),
        },
    }
}
