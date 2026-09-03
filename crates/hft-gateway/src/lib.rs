#![forbid(unsafe_code)]

use hft_book::{BookState, BookStateError, CancelledOrder, OrderBook, TopLevel};
use hft_io::RxFrame;
use hft_risk::{RiskEngine, RiskEngineState, RiskStateError};
use hft_types::{
    Command, InstrumentId, MatchSummary, OrderId, Quantity, RejectReason, ReplaceOrder,
    ReportBuffer, SequenceNumber, Side,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayState<const LEVELS: usize, const ORDERS_PER_LEVEL: usize> {
    pub instrument: InstrumentId,
    pub book: BookState<LEVELS, ORDERS_PER_LEVEL>,
    pub risk: RiskEngineState,
    pub expected_sequence: SequenceNumber,
    pub maximum_received_order_id: Option<OrderId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatewayStateError {
    Book(BookStateError),
    Risk(RiskStateError),
    ExpectedSequence,
    OrderSequence,
    OrderWatermark,
    RiskWatermark,
    MissingReservation,
    ReservationMismatch,
    OrderLimit,
    OrphanReservation,
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

    #[must_use]
    pub fn export_state(&self) -> GatewayState<LEVELS, ORDERS_PER_LEVEL> {
        let book = self.book.export_state();
        GatewayState {
            instrument: book.instrument,
            book,
            risk: self.risk.export_state(),
            expected_sequence: self.expected_sequence,
            maximum_received_order_id: self.maximum_received_order_id,
        }
    }

    /// Rebuilds a gateway from validated logical state.
    ///
    /// # Errors
    ///
    /// Returns the first leaf-state or cross-component invariant failure.
    pub fn from_state(
        state: &GatewayState<LEVELS, ORDERS_PER_LEVEL>,
    ) -> Result<Self, GatewayStateError> {
        let book = OrderBook::from_state(state.instrument, &state.book)
            .map_err(GatewayStateError::Book)?;
        let risk = RiskEngine::from_state(&state.risk).map_err(GatewayStateError::Risk)?;
        Self::validate_cross_component_state(state)?;
        Ok(Self {
            risk,
            book,
            expected_sequence: state.expected_sequence,
            maximum_received_order_id: state.maximum_received_order_id,
        })
    }

    fn validate_cross_component_state(
        state: &GatewayState<LEVELS, ORDERS_PER_LEVEL>,
    ) -> Result<(), GatewayStateError> {
        if state.expected_sequence.0 == 0 {
            return Err(GatewayStateError::ExpectedSequence);
        }
        if state.risk.maximum_order_id > state.maximum_received_order_id {
            return Err(GatewayStateError::RiskWatermark);
        }

        let mut book_orders = 0_usize;
        for (levels, level_count) in [
            (&state.book.bids, state.book.bid_level_count),
            (&state.book.asks, state.book.ask_level_count),
        ] {
            for level in levels.iter().take(level_count) {
                for order in level.orders.iter().take(level.order_count) {
                    book_orders += 1;
                    if order.sequence.0 >= state.expected_sequence.0 {
                        return Err(GatewayStateError::OrderSequence);
                    }
                    if state
                        .maximum_received_order_id
                        .is_none_or(|maximum| maximum < order.order_id)
                    {
                        return Err(GatewayStateError::OrderWatermark);
                    }
                    let Some(reservation) = state
                        .risk
                        .reservations
                        .iter()
                        .find(|reservation| reservation.order_id == order.order_id)
                    else {
                        return Err(GatewayStateError::MissingReservation);
                    };
                    if reservation.account_id != order.account_id
                        || reservation.side != order.side
                        || reservation.quantity != order.quantity
                    {
                        return Err(GatewayStateError::ReservationMismatch);
                    }
                    let Some(account) = state
                        .risk
                        .accounts
                        .iter()
                        .find(|account| account.id == order.account_id)
                    else {
                        return Err(GatewayStateError::ReservationMismatch);
                    };
                    let price =
                        u128::try_from(order.price.0).map_err(|_| GatewayStateError::OrderLimit)?;
                    let notional = price
                        .checked_mul(u128::from(order.quantity.0))
                        .ok_or(GatewayStateError::OrderLimit)?;
                    if order.quantity > account.limits.max_quantity
                        || order.price < account.limits.minimum_price
                        || order.price > account.limits.maximum_price
                        || notional > account.limits.max_notional
                    {
                        return Err(GatewayStateError::OrderLimit);
                    }
                }
            }
        }
        if book_orders != state.risk.reservations.len() {
            return Err(GatewayStateError::OrphanReservation);
        }
        Ok(())
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
        self.accept_sequence(message.sequence())?;
        match message {
            BorrowedMessage::NewOrder(message) => {
                self.process_new_order(message.to_owned(), reports)
            }
            BorrowedMessage::CancelOrder(message) => self.process_cancel(message.to_owned()),
            BorrowedMessage::ReplaceOrder(message) => self.process_replace(message.to_owned()),
        }
    }

    /// Processes one normalized command received by a matching shard.
    ///
    /// # Errors
    ///
    /// Returns the exact sequence, risk, book, or internal risk-state failure.
    /// Book rejection rolls back the taker's risk reservation.
    pub fn process_command<const REPORTS: usize>(
        &mut self,
        command: Command,
        reports: &mut ReportBuffer<REPORTS>,
    ) -> Result<GatewayOutcome, GatewayError> {
        reports.clear();
        self.accept_sequence(command.sequence())?;
        match command {
            Command::NewOrder(order) => self.process_new_order(order, reports),
            Command::CancelOrder(cancel) => self.process_cancel(cancel),
            Command::ReplaceOrder(replace) => self.process_replace(replace),
        }
    }

    fn accept_sequence(&mut self, received_sequence: SequenceNumber) -> Result<(), GatewayError> {
        self.check_sequence(received_sequence)?;
        self.expected_sequence.0 = self
            .expected_sequence
            .0
            .checked_add(1)
            .ok_or(GatewayError::RiskState(RejectReason::ArithmeticOverflow))?;
        Ok(())
    }

    /// Checks sequence admission without changing gateway state.
    ///
    /// # Errors
    ///
    /// Returns a mismatch or exhaustion error when the command cannot advance
    /// the sequence.
    pub fn check_sequence(&self, received_sequence: SequenceNumber) -> Result<(), GatewayError> {
        if received_sequence != self.expected_sequence {
            return Err(GatewayError::Sequence {
                expected: self.expected_sequence,
                received: received_sequence,
            });
        }
        self.expected_sequence
            .0
            .checked_add(1)
            .ok_or(GatewayError::RiskState(RejectReason::ArithmeticOverflow))?;
        Ok(())
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

    #[must_use]
    pub const fn instrument(&self) -> InstrumentId {
        self.book.instrument()
    }

    #[must_use]
    pub fn top_level(&self, side: Side) -> Option<TopLevel> {
        self.book.top_level(side)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_risk::RiskLimits;
    use hft_types::{
        AccountId, CancelOrder, Command, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity,
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
    fn normalized_command_matches_frame_processing() {
        let order = order(1, 1, Side::Sell);
        let frame = encode_new_order(order);
        let mut frame_gateway = gateway();
        let mut command_gateway = gateway();
        let mut frame_reports = ReportBuffer::<4>::new();
        let mut command_reports = ReportBuffer::<4>::new();

        let frame_outcome = frame_gateway
            .process_frame(&RxFrame::from_bytes(&frame), &mut frame_reports)
            .expect("frame accepted");
        let command_outcome = command_gateway
            .process_command(Command::NewOrder(order), &mut command_reports)
            .expect("command accepted");

        assert_eq!(command_outcome, frame_outcome);
        assert_eq!(command_reports, frame_reports);
        assert_eq!(
            command_gateway.stable_digest(),
            frame_gateway.stable_digest()
        );
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
    fn sequence_exhaustion_fails_before_state_mutation() {
        let mut state = gateway().export_state();
        state.expected_sequence = SequenceNumber(u64::MAX);
        let mut gateway = Gateway::<2, 8, 4, 4>::from_state(&state).expect("boundary state");
        let before = gateway.export_state();
        let mut reports = ReportBuffer::<4>::new();
        let mut last = order(1, 1, Side::Buy);
        last.sequence = SequenceNumber(u64::MAX);

        assert_eq!(
            gateway.process_command(Command::NewOrder(last), &mut reports),
            Err(GatewayError::RiskState(RejectReason::ArithmeticOverflow))
        );
        assert_eq!(gateway.export_state(), before);
        assert!(reports.is_empty());
    }

    #[test]
    fn state_round_trip_preserves_gateway_digest() {
        let mut gateway = gateway();
        let mut reports = ReportBuffer::<4>::new();
        gateway
            .process_frame(
                &RxFrame::from_bytes(&encode_new_order(order(1, 1, Side::Buy))),
                &mut reports,
            )
            .expect("resting order");

        let state = gateway.export_state();
        let restored = Gateway::<2, 8, 4, 4>::from_state(&state).expect("valid state");
        assert_eq!(restored.export_state(), state);
        assert_eq!(restored.stable_digest(), gateway.stable_digest());
    }

    #[test]
    fn state_restore_rejects_missing_risk_reservation() {
        let mut gateway = gateway();
        let mut reports = ReportBuffer::<4>::new();
        gateway
            .process_frame(
                &RxFrame::from_bytes(&encode_new_order(order(1, 1, Side::Buy))),
                &mut reports,
            )
            .expect("resting order");

        let mut state = gateway.export_state();
        state.risk.reservations.clear();
        assert!(matches!(
            Gateway::<2, 8, 4, 4>::from_state(&state),
            Err(GatewayStateError::Risk(_) | GatewayStateError::MissingReservation)
        ));
    }

    #[test]
    fn state_restore_rejects_order_outside_account_limits() {
        let mut gateway = gateway();
        let mut reports = ReportBuffer::<4>::new();
        gateway
            .process_frame(
                &RxFrame::from_bytes(&encode_new_order(order(1, 1, Side::Buy))),
                &mut reports,
            )
            .expect("resting order");
        let mut state = gateway.export_state();
        state.risk.accounts[0].limits.max_quantity = Quantity(4);
        assert!(matches!(
            Gateway::<2, 8, 4, 4>::from_state(&state),
            Err(GatewayStateError::OrderLimit)
        ));
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
