#![forbid(unsafe_code)]

use hft_gateway::{Gateway, GatewayError};
use hft_io::RxFrame;
use hft_types::ReportBuffer;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplaySummary {
    pub frames: u64,
    pub reports: u64,
    pub digest: u64,
}

/// Replays frames in input order and returns a stable final-state digest.
///
/// # Errors
///
/// Stops at and returns the first gateway rejection.
pub fn replay<
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS_PER_LEVEL: usize,
    const REPORTS: usize,
>(
    gateway: &mut Gateway<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>,
    frames: &[&[u8]],
) -> Result<ReplaySummary, GatewayError> {
    let mut reports = ReportBuffer::<REPORTS>::new();
    let mut report_count = 0_u64;
    for bytes in frames {
        gateway.process_frame(&RxFrame::from_bytes(bytes), &mut reports)?;
        report_count = report_count.wrapping_add(reports.len() as u64);
    }
    Ok(ReplaySummary {
        frames: frames.len() as u64,
        reports: report_count,
        digest: gateway.stable_digest(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_risk::{RiskEngine, RiskLimits};
    use hft_types::{
        AccountId, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, SequenceNumber, Side,
    };
    use hft_wire::encode_new_order;

    fn run() -> ReplaySummary {
        let mut risk = RiskEngine::<2, 8>::new();
        let limits = RiskLimits {
            max_quantity: Quantity(10),
            max_notional: 10_000,
            max_abs_position: Quantity(100),
            max_open_orders: 8,
            minimum_price: PriceTicks(1),
            maximum_price: PriceTicks(1_000),
        };
        risk.register_account(AccountId(1), limits)
            .expect("first account");
        risk.register_account(AccountId(2), limits)
            .expect("second account");
        let mut gateway = Gateway::<2, 8, 4, 4>::new(risk, InstrumentId(7));
        let make = |id, account, side| {
            encode_new_order(NewOrder {
                order_id: OrderId(id),
                account_id: AccountId(account),
                instrument_id: InstrumentId(7),
                price: PriceTicks(100),
                quantity: Quantity(2),
                sequence: SequenceNumber(id),
                side,
            })
        };
        let first = make(1, 1, Side::Sell);
        let second = make(2, 2, Side::Buy);
        replay::<2, 8, 4, 4, 4>(&mut gateway, &[&first, &second]).expect("valid replay")
    }

    #[test]
    fn replay_digest_is_deterministic() {
        assert_eq!(run(), run());
        assert_eq!(run().reports, 1);
    }
}
