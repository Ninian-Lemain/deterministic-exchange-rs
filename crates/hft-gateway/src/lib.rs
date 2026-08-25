#![forbid(unsafe_code)]

use hft_book::{CancelledOrder, OrderBook};
use hft_io::RxFrame;
use hft_risk::RiskEngine;
use hft_types::{
    InstrumentId, MatchSummary, OrderId, Quantity, RejectReason, ReplaceOrder, ReportBuffer,
    SequenceNumber,
};
use hft_wire::{BorrowedMessage, ParseError, parse_message};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatewayError {
    Parse(ParseError),
    Sequence {
        expected: SequenceNumber,
        received: SequenceNumber,
    },
    Risk(RejectReason),
    Book(RejectReason),
    RiskState(RejectReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatewayOutcome {
    NewOrder(MatchSummary),
    Cancelled(CancelledOrder),
    Replaced(hft_book::ReplacedOrder),
}

#[derive(Debug)]
pub struct Gateway<
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS_PER_LEVEL: usize,
> {
    risk: RiskEngine<ACCOUNTS, RISK_ORDERS>,
    book: OrderBook<LEVELS, ORDERS_PER_LEVEL>,
    expected_sequence: SequenceNumber,
    maximum_received_order_id: Option<OrderId>,
}

impl<
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS_PER_LEVEL: usize,
> Gateway<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>
{
    #[must_use]
    pub const fn new(risk: RiskEngine<ACCOUNTS, RISK_ORDERS>, instrument: InstrumentId) -> Self {
        Self {
            risk,
            book: OrderBook::new(instrument),
            expected_sequence: SequenceNumber(1),
            maximum_received_order_id: None,
        }
    }

    /// Parses from the borrowed RX frame, then normalizes the validated order
    /// once into the fixed-size `NewOrder` value used by the matching shard.
    ///
    /// # Errors
    ///
    /// Returns the exact parse, risk, book, or internal risk-state failure.
    /// Book rejection rolls back the taker's risk reservation.
    pub fn process_frame<const REPORTS: usize>(
        &mut self,
        frame: &RxFrame<'_>,
        reports: &mut ReportBuffer<REPORTS>,
    ) -> Result<GatewayOutcome, GatewayError> {
        reports.clear();
        let message = parse_message(frame).map_err(GatewayError::Parse)?;
        let received_sequence = message.sequence();
        if received_sequence != self.expected_sequence {
            return Err(GatewayError::Sequence {
                expected: self.expected_sequence,
                received: received_sequence,
            });
        }
        self.expected_sequence.0 = self
            .expected_sequence
            .0
            .checked_add(1)
            .ok_or(GatewayError::RiskState(RejectReason::ArithmeticOverflow))?;
        match message {
            BorrowedMessage::NewOrder(message) => {
                self.process_new_order(message.to_owned(), reports)
            }
            BorrowedMessage::CancelOrder(message) => self.process_cancel(message.to_owned()),
            BorrowedMessage::ReplaceOrder(message) => self.process_replace(message.to_owned()),
        }
    }

    fn process_new_order<const REPORTS: usize>(
        &mut self,
        order: hft_types::NewOrder,
        reports: &mut ReportBuffer<REPORTS>,
    ) -> Result<GatewayOutcome, GatewayError> {
        if self
            .maximum_received_order_id
            .is_some_and(|maximum| order.order_id <= maximum)
        {
            return Err(GatewayError::Risk(RejectReason::DuplicateOrderId));
        }
        self.maximum_received_order_id = Some(order.order_id);
        self.risk
            .check_and_reserve(order)
            .map_err(GatewayError::Risk)?;
        let summary = match self.book.submit(order, reports) {
            Ok(summary) => summary,
            Err(error) => {
                self.risk
                    .settle(order.order_id, Quantity(0))
                    .map_err(GatewayError::RiskState)?;
                return Err(GatewayError::Book(error));
            }
        };
        for report in reports.iter() {
            self.risk
                .record_fill(report.maker_order_id, report.quantity)
                .map_err(GatewayError::RiskState)?;
            self.risk
                .record_fill(report.taker_order_id, report.quantity)
                .map_err(GatewayError::RiskState)?;
        }
        // Nothing rests behind an IOC or FOK order: any untraded reservation
        // remainder is discarded exactly like a full cancel.
        if summary.resting_quantity.0 == 0 && summary.filled_quantity.0 < order.quantity.0 {
            self.risk
                .settle(order.order_id, Quantity(0))
                .map_err(GatewayError::RiskState)?;
        }
        Ok(GatewayOutcome::NewOrder(summary))
    }

    /// Owned amend: risk reservation adjusts first (limit-checked), then the
    /// book mutates; a book rejection restores the prior reservation total.
    fn process_replace(&mut self, replace: ReplaceOrder) -> Result<GatewayOutcome, GatewayError> {
        self.risk
            .can_cancel(replace.order_id, replace.account_id)
            .map_err(GatewayError::Risk)?;
        let (_, prior_remaining) = self
            .risk
            .adjust_reservation(
                replace.order_id,
                replace.account_id,
                replace.price,
                replace.quantity,
            )
            .map_err(GatewayError::Risk)?;
        match self.book.replace(replace) {
            Ok(replaced) => Ok(GatewayOutcome::Replaced(replaced)),
            Err(error) => {
                // Unconditional rollback at the prior quantity: the prior
                // state provably passed limits, so the rollback must not
                // fail on a limit check at a different price.
                self.risk
                    .restore_reservation(replace.order_id, replace.account_id, prior_remaining)
                    .map_err(GatewayError::RiskState)?;
                Err(GatewayError::Book(error))
            }
        }
    }
    fn process_cancel(
        &mut self,
        cancel: hft_types::CancelOrder,
    ) -> Result<GatewayOutcome, GatewayError> {
        self.risk
            .can_cancel(cancel.order_id, cancel.account_id)
            .map_err(GatewayError::Risk)?;
        let cancelled = self.book.cancel(cancel).map_err(GatewayError::Book)?;
        let released = self
            .risk
            .cancel_reservation(cancel.order_id, cancel.account_id)
            .map_err(GatewayError::RiskState)?;
        if released != cancelled.quantity {
            return Err(GatewayError::RiskState(RejectReason::ArithmeticOverflow));
        }
        Ok(GatewayOutcome::Cancelled(cancelled))
    }

    #[must_use]
    pub fn stable_digest(&self) -> u64 {
        let session = self.expected_sequence.0.rotate_left(29)
            ^ self
                .maximum_received_order_id
                .map_or(0, |order_id| order_id.0)
                .rotate_left(43);
        self.book.stable_digest().rotate_left(17) ^ self.risk.stable_digest() ^ session
    }

    #[must_use]
    pub const fn risk(&self) -> &RiskEngine<ACCOUNTS, RISK_ORDERS> {
        &self.risk
    }

    #[must_use]
    pub const fn expected_sequence(&self) -> SequenceNumber {
        self.expected_sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_risk::RiskLimits;
    use hft_types::{
        AccountId, CancelOrder, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity,
        SequenceNumber, Side,
    };
    use hft_wire::{encode_cancel_order, encode_new_order, encode_replace_order};

    fn gateway() -> Gateway<2, 8, 4, 4> {
        let mut risk = RiskEngine::new();
        let limits = RiskLimits {
            max_quantity: Quantity(100),
            max_notional: 100_000,
            max_abs_position: Quantity(1_000),
            max_open_orders: 8,
            minimum_price: PriceTicks(1),
            maximum_price: PriceTicks(1_000),
        };
        risk.register_account(AccountId(1), limits)
            .expect("account one");
        risk.register_account(AccountId(2), limits)
            .expect("account two");
        Gateway::new(risk, InstrumentId(1))
    }

    fn order(id: u64, account: u32, side: Side) -> NewOrder {
        NewOrder {
            time_in_force: hft_types::TimeInForce::Gtc,
            order_id: OrderId(id),
            account_id: AccountId(account),
            instrument_id: InstrumentId(1),
            price: PriceTicks(100),
            quantity: Quantity(5),
            sequence: SequenceNumber(id),
            side,
        }
    }

    #[test]
    fn frame_to_execution_report() {
        let mut gateway = gateway();
        let mut reports = ReportBuffer::<4>::new();
        let sell = encode_new_order(order(1, 1, Side::Sell));
        gateway
            .process_frame(&RxFrame::from_bytes(&sell), &mut reports)
            .expect("rest sell");
        let buy = encode_new_order(order(2, 2, Side::Buy));
        let result = gateway
            .process_frame(&RxFrame::from_bytes(&buy), &mut reports)
            .expect("match buy");
        let GatewayOutcome::NewOrder(result) = result else {
            panic!("expected new-order outcome");
        };
        assert_eq!(result.filled_quantity, Quantity(5));
        assert_eq!(reports.len(), 1);
        assert_eq!(gateway.risk().account_snapshot(AccountId(1)), Some((-5, 0)));
        assert_eq!(gateway.risk().account_snapshot(AccountId(2)), Some((5, 0)));
    }

    #[test]
    fn sequence_gap_fails_closed_without_advancing() {
        let mut gateway = gateway();
        let mut reports = ReportBuffer::<4>::new();
        let mut skipped = order(1, 1, Side::Sell);
        skipped.sequence = SequenceNumber(2);
        let bytes = encode_new_order(skipped);
        assert_eq!(
            gateway.process_frame(&RxFrame::from_bytes(&bytes), &mut reports),
            Err(GatewayError::Sequence {
                expected: SequenceNumber(1),
                received: SequenceNumber(2),
            })
        );
        assert_eq!(gateway.expected_sequence(), SequenceNumber(1));
    }

    #[test]
    fn owner_can_cancel_and_release_risk() {
        let mut gateway = gateway();
        let mut reports = ReportBuffer::<4>::new();
        let sell = encode_new_order(order(1, 1, Side::Sell));
        gateway
            .process_frame(&RxFrame::from_bytes(&sell), &mut reports)
            .expect("rest sell");
        let cancel = encode_cancel_order(CancelOrder {
            order_id: OrderId(1),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(2),
        });
        let result = gateway
            .process_frame(&RxFrame::from_bytes(&cancel), &mut reports)
            .expect("cancel order");
        let GatewayOutcome::Cancelled(cancelled) = result else {
            panic!("expected cancellation");
        };
        assert_eq!(cancelled.quantity, Quantity(5));
        assert_eq!(gateway.risk().account_snapshot(AccountId(1)), Some((0, 0)));
    }

    #[test]
    fn rejected_replace_restores_reservation_exactly() {
        let mut gateway = gateway();
        let mut reports = ReportBuffer::<4>::new();
        let sell = encode_new_order(order(1, 1, Side::Sell));
        gateway
            .process_frame(&RxFrame::from_bytes(&sell), &mut reports)
            .expect("rest sell");
        assert_eq!(
            gateway.risk().account_snapshot(AccountId(1)),
            Some((-5, 1)),
            "one open sell reservation of five"
        );
        // Rest an opposing bid so a repriced sell would cross.
        let hedge = encode_new_order({
            let mut order = order(2, 2, Side::Buy);
            order.price = PriceTicks(98);
            order.quantity = Quantity(3);
            order
        });
        gateway
            .process_frame(&RxFrame::from_bytes(&hedge), &mut reports)
            .expect("rest bid");
        // Price drops below the bid (would cross) while quantity increases:
        // risk accepts the larger reservation, the book rejects the cross,
        // and the rollback must restore the original five-unit reservation.
        let crosser = ReplaceOrder {
            order_id: OrderId(1),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(3),
            price: PriceTicks(97),
            quantity: Quantity(8),
        };
        let bytes = encode_replace_order(crosser);
        assert_eq!(
            gateway.process_frame(&RxFrame::from_bytes(&bytes), &mut reports),
            Err(GatewayError::Book(RejectReason::ReplaceWouldCross)),
        );
        assert_eq!(
            gateway.risk().account_snapshot(AccountId(1)),
            Some((-5, 1)),
            "failed replace leaves the original reservation"
        );
        assert_eq!(gateway.risk().account_snapshot(AccountId(2)), Some((3, 1)));
        // Terminal immutability: after a fill, replaces reject as unknown.
        let buy = encode_new_order({
            let mut order = order(3, 2, Side::Buy);
            order.sequence = SequenceNumber(4);
            order
        });
        gateway
            .process_frame(&RxFrame::from_bytes(&buy), &mut reports)
            .expect("crossing taker fills");
        let late = ReplaceOrder {
            order_id: OrderId(1),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(5),
            price: PriceTicks(100),
            quantity: Quantity(5),
        };
        let bytes = encode_replace_order(late);
        assert_eq!(
            gateway.process_frame(&RxFrame::from_bytes(&bytes), &mut reports),
            Err(GatewayError::Risk(RejectReason::UnknownOrder)),
            "filled orders are immutable"
        );
    }

    #[test]
    fn business_rejected_order_id_cannot_be_reused() {
        let mut gateway = gateway();
        let mut reports = ReportBuffer::<4>::new();
        let mut rejected = order(10, 1, Side::Sell);
        rejected.quantity = Quantity(101);
        rejected.sequence = SequenceNumber(1);
        assert_eq!(
            gateway.process_frame(
                &RxFrame::from_bytes(&encode_new_order(rejected)),
                &mut reports
            ),
            Err(GatewayError::Risk(RejectReason::QuantityLimit))
        );
        let mut reused = order(10, 1, Side::Sell);
        reused.sequence = SequenceNumber(2);
        assert_eq!(
            gateway.process_frame(
                &RxFrame::from_bytes(&encode_new_order(reused)),
                &mut reports
            ),
            Err(GatewayError::Risk(RejectReason::DuplicateOrderId))
        );
    }
}
