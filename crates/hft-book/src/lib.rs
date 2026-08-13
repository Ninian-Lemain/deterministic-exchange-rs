#![forbid(unsafe_code)]

use hft_types::{
    AccountId, CancelOrder, ExecutionReport, InstrumentId, MatchSummary, NewOrder, OrderId,
    OrderState, PriceTicks, Quantity, RejectReason, ReportBuffer, SequenceNumber, Side,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RestingOrder {
    id: OrderId,
    account_id: AccountId,
    price: PriceTicks,
    quantity: Quantity,
    sequence: SequenceNumber,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PriceLevel<const ORDERS: usize> {
    price: PriceTicks,
    orders: [Option<RestingOrder>; ORDERS],
    len: usize,
}

impl<const ORDERS: usize> PriceLevel<ORDERS> {
    const fn new(price: PriceTicks) -> Self {
        Self {
            price,
            orders: [None; ORDERS],
            len: 0,
        }
    }

    fn push(&mut self, order: RestingOrder) -> Result<(), RejectReason> {
        let Some(slot) = self.orders.get_mut(self.len) else {
            return Err(RejectReason::PriceLevelOrderCapacity);
        };
        *slot = Some(order);
        self.len += 1;
        Ok(())
    }

    fn pop_front(&mut self) {
        if self.len == 0 {
            return;
        }
        for index in 1..self.len {
            self.orders[index - 1] = self.orders[index];
        }
        self.len -= 1;
        self.orders[self.len] = None;
    }

    fn remove(&mut self, index: usize) -> Option<RestingOrder> {
        if index >= self.len {
            return None;
        }
        let removed = self.orders[index]?;
        for position in (index + 1)..self.len {
            self.orders[position - 1] = self.orders[position];
        }
        self.len -= 1;
        self.orders[self.len] = None;
        Some(removed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelledOrder {
    pub order_id: OrderId,
    pub account_id: AccountId,
    pub quantity: Quantity,
}

/// Single-instrument, fixed-capacity price-time book for a single writer.
#[derive(Debug)]
pub struct OrderBook<const LEVELS: usize, const ORDERS_PER_LEVEL: usize> {
    instrument: InstrumentId,
    bids: [Option<PriceLevel<ORDERS_PER_LEVEL>>; LEVELS],
    asks: [Option<PriceLevel<ORDERS_PER_LEVEL>>; LEVELS],
}

impl<const LEVELS: usize, const ORDERS_PER_LEVEL: usize> OrderBook<LEVELS, ORDERS_PER_LEVEL> {
    #[must_use]
    pub const fn new(instrument: InstrumentId) -> Self {
        Self {
            instrument,
            bids: [None; LEVELS],
            asks: [None; LEVELS],
        }
    }

    /// # Errors
    ///
    /// Returns an explicit validation or capacity rejection. All capacity
    /// requirements are preflighted so such rejection leaves the book intact.
    pub fn submit<const REPORTS: usize>(
        &mut self,
        order: NewOrder,
        reports: &mut ReportBuffer<REPORTS>,
    ) -> Result<MatchSummary, RejectReason> {
        let initial_report_count = reports.len();
        self.preflight(order, reports.remaining_capacity())?;
        let original = order.quantity.0;
        let mut remaining = original;

        while remaining > 0 {
            let Some(level_index) = self.best_crossing_level(order.side, order.price) else {
                break;
            };
            let levels = match order.side {
                Side::Buy => &mut self.asks,
                Side::Sell => &mut self.bids,
            };
            let Some(level) = levels[level_index].as_mut() else {
                return Err(RejectReason::ArithmeticOverflow);
            };
            let Some(mut maker) = level.orders[0] else {
                return Err(RejectReason::ArithmeticOverflow);
            };
            let traded = remaining.min(maker.quantity.0);
            reports.push(ExecutionReport {
                maker_order_id: maker.id,
                taker_order_id: order.order_id,
                instrument_id: order.instrument_id,
                price: maker.price,
                quantity: Quantity(traded),
                sequence: order.sequence,
            })?;
            remaining = remaining
                .checked_sub(traded)
                .ok_or(RejectReason::ArithmeticOverflow)?;
            maker.quantity.0 = maker
                .quantity
                .0
                .checked_sub(traded)
                .ok_or(RejectReason::ArithmeticOverflow)?;
            if maker.quantity.0 == 0 {
                level.pop_front();
                if level.len == 0 {
                    levels[level_index] = None;
                }
            } else {
                level.orders[0] = Some(maker);
            }
        }

        let filled = original
            .checked_sub(remaining)
            .ok_or(RejectReason::ArithmeticOverflow)?;
        if remaining > 0 {
            self.rest(order, Quantity(remaining))?;
        }
        let state = if remaining == 0 {
            OrderState::Filled
        } else if filled == 0 {
            OrderState::Accepted
        } else {
            OrderState::PartiallyFilled
        };
        Ok(MatchSummary {
            state,
            filled_quantity: Quantity(filled),
            resting_quantity: Quantity(remaining),
            report_count: reports.len() - initial_report_count,
        })
    }

    /// Removes an owned resting order while retaining FIFO order for all peers.
    ///
    /// # Errors
    ///
    /// Returns an instrument, unknown-order, or ownership rejection without
    /// mutating the book.
    pub fn cancel(&mut self, cancel: CancelOrder) -> Result<CancelledOrder, RejectReason> {
        if cancel.instrument_id != self.instrument {
            return Err(RejectReason::InvalidInstrument);
        }
        let (side, level_index, order_index, owner, quantity) = self
            .find_order(cancel.order_id)
            .ok_or(RejectReason::UnknownOrder)?;
        if owner != cancel.account_id {
            return Err(RejectReason::NotOrderOwner);
        }
        let levels = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let level = levels[level_index]
            .as_mut()
            .ok_or(RejectReason::UnknownOrder)?;
        let removed = level
            .remove(order_index)
            .ok_or(RejectReason::UnknownOrder)?;
        if level.len == 0 {
            levels[level_index] = None;
        }
        debug_assert_eq!(removed.quantity, quantity);
        Ok(CancelledOrder {
            order_id: removed.id,
            account_id: removed.account_id,
            quantity: removed.quantity,
        })
    }

    fn preflight(&self, order: NewOrder, report_capacity: usize) -> Result<(), RejectReason> {
        if order.instrument_id != self.instrument {
            return Err(RejectReason::InvalidInstrument);
        }
        if order.price.0 <= 0 {
            return Err(RejectReason::InvalidPrice);
        }
        if order.quantity.0 == 0 {
            return Err(RejectReason::InvalidQuantity);
        }
        if self.contains_order(order.order_id) {
            return Err(RejectReason::DuplicateOrderId);
        }

        let (remaining, report_count) = self.simulate_matches(order)?;
        if report_count > report_capacity {
            return Err(RejectReason::ReportCapacity);
        }
        if remaining > 0 {
            let levels = match order.side {
                Side::Buy => &self.bids,
                Side::Sell => &self.asks,
            };
            if let Some(level) = levels
                .iter()
                .flatten()
                .find(|level| level.price == order.price)
            {
                if level.len == ORDERS_PER_LEVEL {
                    return Err(RejectReason::PriceLevelOrderCapacity);
                }
            } else if levels.iter().all(Option::is_some) {
                return Err(RejectReason::PriceLevelCapacity);
            }
        }
        Ok(())
    }

    fn simulate_matches(&self, order: NewOrder) -> Result<(u64, usize), RejectReason> {
        self.simulate_sorted(order)
    }

    fn simulate_sorted(&self, order: NewOrder) -> Result<(u64, usize), RejectReason> {
        let levels = match order.side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };
        let mut remaining = order.quantity.0;
        let mut reports = 0_usize;
        let mut boundary: Option<PriceTicks> = None;
        loop {
            let next = levels
                .iter()
                .flatten()
                .filter(|level| match order.side {
                    Side::Buy => {
                        level.price.0 <= order.price.0
                            && boundary.is_none_or(|price| level.price.0 > price.0)
                    }
                    Side::Sell => {
                        level.price.0 >= order.price.0
                            && boundary.is_none_or(|price| level.price.0 < price.0)
                    }
                })
                .min_by_key(|level| match order.side {
                    Side::Buy => level.price.0,
                    Side::Sell => -level.price.0,
                });
            let Some(level) = next else {
                break;
            };
            boundary = Some(level.price);
            for maker in level.orders[..level.len].iter().flatten() {
                if remaining == 0 {
                    break;
                }
                remaining -= remaining.min(maker.quantity.0);
                reports = reports
                    .checked_add(1)
                    .ok_or(RejectReason::ArithmeticOverflow)?;
            }
            if remaining == 0 {
                break;
            }
        }
        Ok((remaining, reports))
    }

    fn best_crossing_level(&self, side: Side, price: PriceTicks) -> Option<usize> {
        let levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };
        levels
            .iter()
            .enumerate()
            .filter_map(|(index, level)| level.as_ref().map(|level| (index, level)))
            .filter(|(_, level)| match side {
                Side::Buy => level.price.0 <= price.0,
                Side::Sell => level.price.0 >= price.0,
            })
            .min_by_key(|(_, level)| match side {
                Side::Buy => level.price.0,
                Side::Sell => -level.price.0,
            })
            .map(|(index, _)| index)
    }

    fn rest(&mut self, order: NewOrder, quantity: Quantity) -> Result<(), RejectReason> {
        let levels = match order.side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let resting = RestingOrder {
            id: order.order_id,
            account_id: order.account_id,
            price: order.price,
            quantity,
            sequence: order.sequence,
        };
        if let Some(level) = levels
            .iter_mut()
            .flatten()
            .find(|level| level.price == order.price)
        {
            return level.push(resting);
        }
        let slot = levels
            .iter_mut()
            .find(|level| level.is_none())
            .ok_or(RejectReason::PriceLevelCapacity)?;
        let mut level = PriceLevel::new(order.price);
        level.push(resting)?;
        *slot = Some(level);
        Ok(())
    }

    fn contains_order(&self, order_id: OrderId) -> bool {
        self.bids
            .iter()
            .chain(&self.asks)
            .flatten()
            .flat_map(|level| level.orders[..level.len].iter().flatten())
            .any(|order| order.id == order_id)
    }

    fn find_order(&self, order_id: OrderId) -> Option<(Side, usize, usize, AccountId, Quantity)> {
        for (side, levels) in [(Side::Buy, &self.bids), (Side::Sell, &self.asks)] {
            for (level_index, level) in levels.iter().enumerate() {
                let Some(level) = level else {
                    continue;
                };
                for (order_index, order) in level.orders[..level.len].iter().enumerate() {
                    if let Some(order) = order {
                        if order.id == order_id {
                            return Some((
                                side,
                                level_index,
                                order_index,
                                order.account_id,
                                order.quantity,
                            ));
                        }
                    }
                }
            }
        }
        None
    }

    #[must_use]
    pub fn stable_digest(&self) -> u64 {
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        mix(&mut digest, u64::from(self.instrument.0));
        self.digest_side(&mut digest, Side::Buy);
        self.digest_side(&mut digest, Side::Sell);
        digest
    }

    fn digest_side(&self, digest: &mut u64, side: Side) {
        mix(digest, u64::from(side as u8));
        let levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        let mut boundary: Option<PriceTicks> = None;
        loop {
            let next = levels
                .iter()
                .flatten()
                .filter(|level| boundary.is_none_or(|price| level.price.0 > price.0))
                .min_by_key(|level| level.price.0);
            let Some(level) = next else {
                break;
            };
            boundary = Some(level.price);
            mix(digest, u64::from_be_bytes(level.price.0.to_be_bytes()));
            for order in level.orders[..level.len].iter().flatten() {
                mix(digest, order.id.0);
                mix(digest, u64::from(order.account_id.0));
                mix(digest, order.quantity.0);
                mix(digest, order.sequence.0);
            }
        }
    }

    #[must_use]
    pub fn order_count(&self) -> usize {
        self.bids
            .iter()
            .chain(&self.asks)
            .flatten()
            .map(|level| level.len)
            .sum()
    }
}

fn mix(digest: &mut u64, value: u64) {
    *digest ^= value;
    *digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_types::AccountId;

    fn order(id: u64, price: i64, quantity: u64, side: Side) -> NewOrder {
        NewOrder {
            order_id: OrderId(id),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            price: PriceTicks(price),
            quantity: Quantity(quantity),
            sequence: SequenceNumber(id),
            side,
        }
    }

    #[test]
    fn matches_price_then_time() {
        let mut book = OrderBook::<4, 4>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<4>::new();
        book.submit(order(1, 101, 2, Side::Sell), &mut reports)
            .expect("rest ask");
        reports.clear();
        book.submit(order(2, 100, 2, Side::Sell), &mut reports)
            .expect("rest better ask");
        reports.clear();
        book.submit(order(3, 100, 2, Side::Sell), &mut reports)
            .expect("rest same ask");
        reports.clear();
        let summary = book
            .submit(order(4, 101, 5, Side::Buy), &mut reports)
            .expect("cross asks");
        let makers: std::vec::Vec<_> = reports.iter().map(|report| report.maker_order_id).collect();
        assert_eq!(makers, [OrderId(2), OrderId(3), OrderId(1)]);
        assert_eq!(summary.filled_quantity, Quantity(5));
        assert_eq!(book.order_count(), 1);
    }

    #[test]
    fn report_capacity_preflight_preserves_book() {
        let mut book = OrderBook::<2, 2>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<1>::new();
        book.submit(order(1, 100, 1, Side::Sell), &mut reports)
            .expect("first ask");
        reports.clear();
        book.submit(order(2, 100, 1, Side::Sell), &mut reports)
            .expect("second ask");
        reports.clear();
        let digest = book.stable_digest();
        assert_eq!(
            book.submit(order(3, 100, 2, Side::Buy), &mut reports),
            Err(RejectReason::ReportCapacity)
        );
        assert_eq!(book.stable_digest(), digest);
    }

    #[test]
    fn deterministic_runs_match_digest() {
        let mut first = OrderBook::<4, 4>::new(InstrumentId(1));
        let mut second = OrderBook::<4, 4>::new(InstrumentId(1));
        for book in [&mut first, &mut second] {
            let mut reports = ReportBuffer::<4>::new();
            for input in [
                order(1, 99, 2, Side::Buy),
                order(2, 101, 3, Side::Sell),
                order(3, 101, 1, Side::Buy),
            ] {
                reports.clear();
                book.submit(input, &mut reports).expect("valid order");
            }
        }
        assert_eq!(first.stable_digest(), second.stable_digest());
    }

    #[test]
    fn cancel_requires_owner_and_preserves_fifo() {
        let mut book = OrderBook::<2, 4>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<4>::new();
        for resting in [
            order(1, 100, 1, Side::Sell),
            order(2, 100, 1, Side::Sell),
            order(3, 100, 1, Side::Sell),
        ] {
            reports.clear();
            book.submit(resting, &mut reports).expect("rest order");
        }
        assert_eq!(
            book.cancel(CancelOrder {
                order_id: OrderId(2),
                account_id: AccountId(9),
                instrument_id: InstrumentId(1),
                sequence: SequenceNumber(4),
            }),
            Err(RejectReason::NotOrderOwner)
        );
        book.cancel(CancelOrder {
            order_id: OrderId(2),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(4),
        })
        .expect("owner cancel");
        reports.clear();
        book.submit(order(4, 100, 2, Side::Buy), &mut reports)
            .expect("cross remaining FIFO");
        let makers: std::vec::Vec<_> = reports.iter().map(|report| report.maker_order_id).collect();
        assert_eq!(makers, [OrderId(1), OrderId(3)]);
    }
}
