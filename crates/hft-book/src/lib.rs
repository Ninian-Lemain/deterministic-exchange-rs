#![forbid(unsafe_code)]

use hft_types::{
    AccountId, CancelOrder, ExecutionReport, InstrumentId, MatchSummary, NewOrder, OrderId,
    OrderState, PriceTicks, Quantity, RejectReason, ReportBuffer, SequenceNumber, Side,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RestingOrder {
    id: OrderId,
    account_id: AccountId,
    index_slot: u32,
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

const ORDER_INDEX_PLANES: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OrderLocation {
    side: Side,
    level_index: usize,
    order_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexSlot {
    Empty,
    Occupied {
        order_id: OrderId,
        location: OrderLocation,
    },
}

#[derive(Debug)]
struct OrderIndex<const LEVELS: usize, const ORDERS: usize> {
    slots: [[[IndexSlot; ORDERS]; LEVELS]; ORDER_INDEX_PLANES],
}

impl<const LEVELS: usize, const ORDERS: usize> OrderIndex<LEVELS, ORDERS> {
    const fn new() -> Self {
        Self {
            slots: [[[IndexSlot::Empty; ORDERS]; LEVELS]; ORDER_INDEX_PLANES],
        }
    }

    fn capacity() -> Option<usize> {
        LEVELS.checked_mul(ORDERS)?.checked_mul(ORDER_INDEX_PLANES)
    }

    fn coordinates(flat_index: usize) -> Option<(usize, usize, usize)> {
        let per_plane = LEVELS.checked_mul(ORDERS)?;
        if per_plane == 0 {
            return None;
        }
        let plane = flat_index / per_plane;
        let within_plane = flat_index % per_plane;
        Some((plane, within_plane / ORDERS, within_plane % ORDERS))
    }

    fn slot(&self, flat_index: usize) -> Option<&IndexSlot> {
        let (plane, level, order) = Self::coordinates(flat_index)?;
        self.slots.get(plane)?.get(level)?.get(order)
    }

    fn slot_mut(&mut self, flat_index: usize) -> Option<&mut IndexSlot> {
        let (plane, level, order) = Self::coordinates(flat_index)?;
        self.slots.get_mut(plane)?.get_mut(level)?.get_mut(order)
    }

    fn probe_start(order_id: OrderId, capacity: usize) -> usize {
        let capacity_u64 = u64::try_from(capacity).unwrap_or(u64::MAX);
        let mixed = order_id.0.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        usize::try_from(mixed % capacity_u64).unwrap_or(0)
    }

    fn find_slot(&self, order_id: OrderId) -> Option<usize> {
        let capacity = Self::capacity()?;
        if capacity == 0 {
            return None;
        }
        let start = Self::probe_start(order_id, capacity);
        for offset in 0..capacity {
            let slot_index = start.wrapping_add(offset) % capacity;
            match self.slot(slot_index)? {
                IndexSlot::Empty => return None,
                IndexSlot::Occupied {
                    order_id: indexed, ..
                } if *indexed == order_id => return Some(slot_index),
                IndexSlot::Occupied { .. } => {}
            }
        }
        None
    }

    fn location(&self, order_id: OrderId) -> Option<OrderLocation> {
        match self.slot(self.find_slot(order_id)?)? {
            IndexSlot::Occupied { location, .. } => Some(*location),
            IndexSlot::Empty => None,
        }
    }

    fn insert(&mut self, order_id: OrderId, location: OrderLocation) -> Result<u32, RejectReason> {
        let capacity = Self::capacity().ok_or(RejectReason::OrderCapacity)?;
        if capacity == 0 {
            return Err(RejectReason::OrderCapacity);
        }
        let start = Self::probe_start(order_id, capacity);
        for offset in 0..capacity {
            let slot_index = start.wrapping_add(offset) % capacity;
            match *self.slot(slot_index).ok_or(RejectReason::OrderCapacity)? {
                IndexSlot::Empty => {
                    let slot_id =
                        u32::try_from(slot_index).map_err(|_| RejectReason::OrderCapacity)?;
                    *self
                        .slot_mut(slot_index)
                        .ok_or(RejectReason::OrderCapacity)? =
                        IndexSlot::Occupied { order_id, location };
                    return Ok(slot_id);
                }
                IndexSlot::Occupied {
                    order_id: indexed, ..
                } if indexed == order_id => return Err(RejectReason::DuplicateOrderId),
                IndexSlot::Occupied { .. } => {}
            }
        }
        Err(RejectReason::OrderCapacity)
    }

    fn remove_at<F>(
        &mut self,
        slot_index: u32,
        order_id: OrderId,
        mut update_reverse_slot: F,
    ) -> Option<OrderLocation>
    where
        F: FnMut(OrderId, OrderLocation, u32) -> bool,
    {
        let slot_index = usize::try_from(slot_index).ok()?;
        let IndexSlot::Occupied {
            order_id: indexed,
            location,
        } = *self.slot(slot_index)?
        else {
            return None;
        };
        if indexed != order_id {
            return None;
        }
        let capacity = Self::capacity()?;
        let mut hole = slot_index;
        let mut candidate_index = hole.wrapping_add(1) % capacity;
        // Close the probe hole without retaining deletion tombstones.
        loop {
            let candidate = *self.slot(candidate_index)?;
            let IndexSlot::Occupied {
                order_id: candidate_id,
                location: candidate_location,
            } = candidate
            else {
                *self.slot_mut(hole)? = IndexSlot::Empty;
                break;
            };
            let home_bucket = Self::probe_start(candidate_id, capacity);
            if Self::probe_distance(home_bucket, hole, capacity)
                < Self::probe_distance(home_bucket, candidate_index, capacity)
            {
                *self.slot_mut(hole)? = candidate;
                let new_slot = u32::try_from(hole).ok()?;
                let updated = update_reverse_slot(candidate_id, candidate_location, new_slot);
                debug_assert!(updated);
                hole = candidate_index;
            }
            candidate_index = candidate_index.wrapping_add(1) % capacity;
        }
        Some(location)
    }

    fn probe_distance(home: usize, current: usize, capacity: usize) -> usize {
        if current >= home {
            current - home
        } else {
            capacity - (home - current)
        }
    }

    fn update_at(&mut self, slot_index: u32, order_id: OrderId, location: OrderLocation) -> bool {
        let Ok(slot_index) = usize::try_from(slot_index) else {
            return false;
        };
        let Some(IndexSlot::Occupied {
            order_id: indexed_id,
            location: indexed_location,
        }) = self.slot_mut(slot_index)
        else {
            return false;
        };
        if *indexed_id != order_id {
            return false;
        }
        *indexed_location = location;
        true
    }
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

/// Single-instrument, fixed-capacity price-time book with indexed order lookup.
#[derive(Debug)]
pub struct OrderBook<const LEVELS: usize, const ORDERS_PER_LEVEL: usize> {
    instrument: InstrumentId,
    bids: [Option<PriceLevel<ORDERS_PER_LEVEL>>; LEVELS],
    asks: [Option<PriceLevel<ORDERS_PER_LEVEL>>; LEVELS],
    index: OrderIndex<LEVELS, ORDERS_PER_LEVEL>,
}

impl<const LEVELS: usize, const ORDERS_PER_LEVEL: usize> OrderBook<LEVELS, ORDERS_PER_LEVEL> {
    #[must_use]
    pub const fn new(instrument: InstrumentId) -> Self {
        Self {
            instrument,
            bids: [None; LEVELS],
            asks: [None; LEVELS],
            index: OrderIndex::new(),
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
            let (maker_side, mut maker) = self
                .front_maker(order.side, level_index)
                .ok_or(RejectReason::ArithmeticOverflow)?;
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
                let location = OrderLocation {
                    side: maker_side,
                    level_index,
                    order_index: 0,
                };
                let removed_location = self.remove_index_entry(maker.index_slot, maker.id);
                debug_assert_eq!(removed_location, Some(location));
                let (levels, index) = match maker_side {
                    Side::Buy => (&mut self.bids, &mut self.index),
                    Side::Sell => (&mut self.asks, &mut self.index),
                };
                let Some(level) = levels[level_index].as_mut() else {
                    return Err(RejectReason::ArithmeticOverflow);
                };
                level.pop_front();
                for (order_index, shifted) in level.orders[..level.len].iter().flatten().enumerate()
                {
                    let updated = index.update_at(
                        shifted.index_slot,
                        shifted.id,
                        OrderLocation {
                            side: maker_side,
                            level_index,
                            order_index,
                        },
                    );
                    debug_assert!(updated);
                }
                if level.len == 0 {
                    levels[level_index] = None;
                }
            } else {
                let levels = match maker_side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                let Some(level) = levels[level_index].as_mut() else {
                    return Err(RejectReason::ArithmeticOverflow);
                };
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
        let (side, level_index, order_index, owner, quantity, index_slot) = self
            .find_order(cancel.order_id)
            .ok_or(RejectReason::UnknownOrder)?;
        if owner != cancel.account_id {
            return Err(RejectReason::NotOrderOwner);
        }
        let location = OrderLocation {
            side,
            level_index,
            order_index,
        };
        let removed_location = self.remove_index_entry(index_slot, cancel.order_id);
        if removed_location != Some(location) {
            return Err(RejectReason::ArithmeticOverflow);
        }
        let (levels, index) = match side {
            Side::Buy => (&mut self.bids, &mut self.index),
            Side::Sell => (&mut self.asks, &mut self.index),
        };
        let level = levels[level_index]
            .as_mut()
            .ok_or(RejectReason::UnknownOrder)?;
        let removed = level
            .remove(order_index)
            .ok_or(RejectReason::UnknownOrder)?;
        for (shifted_index, shifted) in level.orders[order_index..level.len]
            .iter()
            .flatten()
            .enumerate()
        {
            let updated = index.update_at(
                shifted.index_slot,
                shifted.id,
                OrderLocation {
                    side,
                    level_index,
                    order_index: order_index + shifted_index,
                },
            );
            debug_assert!(updated);
        }
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

    fn front_maker(&self, taker_side: Side, level_index: usize) -> Option<(Side, RestingOrder)> {
        let (maker_side, levels) = match taker_side {
            Side::Buy => (Side::Sell, &self.asks),
            Side::Sell => (Side::Buy, &self.bids),
        };
        let maker = levels
            .get(level_index)?
            .as_ref()?
            .orders
            .first()
            .copied()
            .flatten()?;
        Some((maker_side, maker))
    }

    fn rest(&mut self, order: NewOrder, quantity: Quantity) -> Result<(), RejectReason> {
        let (levels, index) = match order.side {
            Side::Buy => (&mut self.bids, &mut self.index),
            Side::Sell => (&mut self.asks, &mut self.index),
        };
        if let Some(level_index) = levels
            .iter()
            .position(|level| level.is_some_and(|level| level.price == order.price))
        {
            let level = levels[level_index]
                .as_mut()
                .ok_or(RejectReason::PriceLevelCapacity)?;
            let order_index = level.len;
            let location = OrderLocation {
                side: order.side,
                level_index,
                order_index,
            };
            let resting = RestingOrder {
                id: order.order_id,
                account_id: order.account_id,
                index_slot: 0,
                price: order.price,
                quantity,
                sequence: order.sequence,
            };
            level.push(resting)?;
            let index_slot = match index.insert(order.order_id, location) {
                Ok(index_slot) => index_slot,
                Err(error) => {
                    let _ = level.remove(order_index);
                    return Err(error);
                }
            };
            let Some(resting) = level.orders[order_index].as_mut() else {
                let _ = level.remove(order_index);
                return Err(RejectReason::ArithmeticOverflow);
            };
            resting.index_slot = index_slot;
            return Ok(());
        }
        let level_index = levels
            .iter()
            .position(Option::is_none)
            .ok_or(RejectReason::PriceLevelCapacity)?;
        let location = OrderLocation {
            side: order.side,
            level_index,
            order_index: 0,
        };
        let resting = RestingOrder {
            id: order.order_id,
            account_id: order.account_id,
            index_slot: 0,
            price: order.price,
            quantity,
            sequence: order.sequence,
        };
        let mut level = PriceLevel::new(order.price);
        level.push(resting)?;
        levels[level_index] = Some(level);
        let index_slot = match index.insert(order.order_id, location) {
            Ok(index_slot) => index_slot,
            Err(error) => {
                levels[level_index] = None;
                return Err(error);
            }
        };
        let Some(resting) = levels[level_index]
            .as_mut()
            .and_then(|level| level.orders[0].as_mut())
        else {
            levels[level_index] = None;
            return Err(RejectReason::ArithmeticOverflow);
        };
        resting.index_slot = index_slot;
        Ok(())
    }

    fn remove_index_entry(&mut self, index_slot: u32, order_id: OrderId) -> Option<OrderLocation> {
        let (bids, asks, index) = (&mut self.bids, &mut self.asks, &mut self.index);
        index.remove_at(index_slot, order_id, |moved_id, location, new_slot| {
            let levels = match location.side {
                Side::Buy => &mut *bids,
                Side::Sell => &mut *asks,
            };
            let Some(resting) = levels
                .get_mut(location.level_index)
                .and_then(Option::as_mut)
                .and_then(|level| level.orders.get_mut(location.order_index))
                .and_then(Option::as_mut)
            else {
                return false;
            };
            if resting.id != moved_id {
                return false;
            }
            resting.index_slot = new_slot;
            true
        })
    }

    fn contains_order(&self, order_id: OrderId) -> bool {
        self.index.location(order_id).is_some()
    }

    fn find_order(
        &self,
        order_id: OrderId,
    ) -> Option<(Side, usize, usize, AccountId, Quantity, u32)> {
        let location = self.index.location(order_id)?;
        let levels = match location.side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        let order = levels
            .get(location.level_index)?
            .as_ref()?
            .orders
            .get(location.order_index)?
            .as_ref()?;
        if order.id != order_id {
            return None;
        }
        Some((
            location.side,
            location.level_index,
            location.order_index,
            order.account_id,
            order.quantity,
            order.index_slot,
        ))
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

    fn assert_index_consistent<const LEVELS: usize, const ORDERS: usize>(
        book: &OrderBook<LEVELS, ORDERS>,
    ) {
        let mut live_orders = 0_usize;
        for (side, levels) in [(Side::Buy, &book.bids), (Side::Sell, &book.asks)] {
            for (level_index, level) in levels.iter().enumerate() {
                let Some(level) = level else {
                    continue;
                };
                for (order_index, resting) in level.orders[..level.len].iter().enumerate() {
                    let resting = resting.as_ref().expect("dense FIFO level");
                    assert_eq!(
                        book.index.location(resting.id),
                        Some(OrderLocation {
                            side,
                            level_index,
                            order_index,
                        })
                    );
                    assert_eq!(
                        book.index
                            .slot(usize::try_from(resting.index_slot).expect("valid index slot")),
                        Some(&IndexSlot::Occupied {
                            order_id: resting.id,
                            location: OrderLocation {
                                side,
                                level_index,
                                order_index,
                            },
                        })
                    );
                    live_orders += 1;
                }
            }
        }
        let indexed_orders = book
            .index
            .slots
            .iter()
            .flatten()
            .flatten()
            .filter(|slot| matches!(slot, IndexSlot::Occupied { .. }))
            .count();
        assert_eq!(indexed_orders, live_orders);
        assert_eq!(book.order_count(), live_orders);
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

    #[test]
    fn index_handles_collisions_shifts_and_slot_reuse() {
        let mut book = OrderBook::<2, 4>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<4>::new();
        for resting in [
            order(1, 100, 1, Side::Sell),
            order(33, 100, 1, Side::Sell),
            order(65, 100, 1, Side::Sell),
        ] {
            book.submit(resting, &mut reports).expect("rest order");
            reports.clear();
        }
        assert_index_consistent(&book);

        book.cancel(CancelOrder {
            order_id: OrderId(33),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(66),
        })
        .expect("cancel colliding middle order");
        assert_index_consistent(&book);

        book.submit(order(97, 100, 1, Side::Buy), &mut reports)
            .expect("fill front order");
        assert_eq!(
            reports.iter().next().map(|report| report.maker_order_id),
            Some(OrderId(1))
        );
        assert_index_consistent(&book);

        book.cancel(CancelOrder {
            order_id: OrderId(65),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(98),
        })
        .expect("cancel shifted order");
        reports.clear();
        book.submit(order(129, 100, 1, Side::Sell), &mut reports)
            .expect("reuse index slot");
        assert_index_consistent(&book);
    }

    #[test]
    fn back_shift_updates_reverse_slots_across_sides() {
        let mut book = OrderBook::<2, 2>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<1>::new();
        book.submit(order(1, 101, 1, Side::Sell), &mut reports)
            .expect("rest sell");
        book.submit(order(17, 100, 1, Side::Buy), &mut reports)
            .expect("rest colliding buy");
        assert_index_consistent(&book);

        book.cancel(CancelOrder {
            order_id: OrderId(1),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(18),
        })
        .expect("cancel sell before colliding buy");
        assert_index_consistent(&book);

        book.cancel(CancelOrder {
            order_id: OrderId(17),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(19),
        })
        .expect("cancel relocated buy");
        assert_index_consistent(&book);
    }
}
