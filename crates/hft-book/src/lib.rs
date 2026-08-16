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
    prev: usize,
    next: usize,
}

const NIL: usize = usize::MAX;

/// Fixed-capacity slot: a live FIFO node or a free-list link. Live and free
/// sets are disjoint by construction, and a slot handle stays stable while
/// unrelated slots mutate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OrderSlot {
    Free { next_free: usize },
    Live(RestingOrder),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PriceLevel<const ORDERS: usize> {
    price: PriceTicks,
    slots: [OrderSlot; ORDERS],
    head: usize,
    tail: usize,
    free_head: usize,
    len: usize,
}

const ORDER_INDEX_PLANES: usize = 4;

/// `slot` is a stable per-level slot handle, never a FIFO position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OrderLocation {
    side: Side,
    level_index: usize,
    slot: usize,
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
}

impl<const ORDERS: usize> PriceLevel<ORDERS> {
    fn new(price: PriceTicks) -> Self {
        Self {
            price,
            slots: core::array::from_fn(|index| OrderSlot::Free {
                next_free: if index + 1 < ORDERS { index + 1 } else { NIL },
            }),
            head: NIL,
            tail: NIL,
            free_head: if ORDERS == 0 { NIL } else { 0 },
            len: 0,
        }
    }

    /// Appends at the tail and returns the stable slot handle.
    fn push_tail(&mut self, mut order: RestingOrder) -> Result<usize, RejectReason> {
        if self.free_head == NIL || self.len >= ORDERS {
            return Err(RejectReason::PriceLevelOrderCapacity);
        }
        let slot = self.free_head;
        let Some(OrderSlot::Free { next_free }) = self.slots.get(slot).copied() else {
            return Err(RejectReason::PriceLevelOrderCapacity);
        };
        if self.tail != NIL {
            match self.slots.get_mut(self.tail) {
                Some(OrderSlot::Live(tail_order)) => tail_order.next = slot,
                _ => return Err(RejectReason::ArithmeticOverflow),
            }
        }
        order.prev = self.tail;
        order.next = NIL;
        self.slots[slot] = OrderSlot::Live(order);
        self.free_head = next_free;
        if self.tail == NIL {
            self.head = slot;
        }
        self.tail = slot;
        self.len += 1;
        Ok(slot)
    }

    /// Detaches a live slot and returns it to the free list. Stale or free
    /// handles fail closed without mutating the level.
    fn unlink(&mut self, slot: usize) -> Option<RestingOrder> {
        let Some(OrderSlot::Live(order)) = self.slots.get(slot).copied() else {
            return None;
        };
        if order.prev != NIL && !matches!(self.slots.get(order.prev), Some(OrderSlot::Live(_))) {
            return None;
        }
        if order.next != NIL && !matches!(self.slots.get(order.next), Some(OrderSlot::Live(_))) {
            return None;
        }
        match order.prev {
            NIL => self.head = order.next,
            prev => {
                if let Some(OrderSlot::Live(prev_order)) = self.slots.get_mut(prev) {
                    prev_order.next = order.next;
                }
            }
        }
        match order.next {
            NIL => self.tail = order.prev,
            next => {
                if let Some(OrderSlot::Live(next_order)) = self.slots.get_mut(next) {
                    next_order.prev = order.prev;
                }
            }
        }
        self.slots[slot] = OrderSlot::Free {
            next_free: self.free_head,
        };
        self.free_head = slot;
        self.len -= 1;
        Some(order)
    }

    fn front(&self) -> Option<(usize, RestingOrder)> {
        if self.head == NIL {
            return None;
        }
        match self.slots.get(self.head).copied() {
            Some(OrderSlot::Live(order)) => Some((self.head, order)),
            _ => None,
        }
    }

    fn front_mut(&mut self) -> Option<&mut RestingOrder> {
        if self.head == NIL {
            return None;
        }
        match self.slots.get_mut(self.head) {
            Some(OrderSlot::Live(order)) => Some(order),
            _ => None,
        }
    }

    fn get_live(&self, slot: usize) -> Option<&RestingOrder> {
        match self.slots.get(slot) {
            Some(OrderSlot::Live(order)) => Some(order),
            _ => None,
        }
    }

    fn get_live_mut(&mut self, slot: usize) -> Option<&mut RestingOrder> {
        match self.slots.get_mut(slot) {
            Some(OrderSlot::Live(order)) => Some(order),
            _ => None,
        }
    }

    fn for_each_live(&self, mut visit: impl FnMut(&RestingOrder)) {
        let mut cursor = self.head;
        while cursor != NIL {
            let Some(OrderSlot::Live(order)) = self.slots.get(cursor) else {
                break;
            };
            visit(order);
            cursor = order.next;
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CancelledOrder {
    pub order_id: OrderId,
    pub account_id: AccountId,
    pub quantity: Quantity,
}

/// Sorted index of occupied level indices. Bids are sorted by price
/// descending (best bid first); asks are sorted by price ascending (best ask
/// first). This gives O(1) best-price discovery and O(n) insertion/removal
/// where n is the number of active levels (always ≤ LEVELS).
#[derive(Debug)]
struct LevelIndex<const LEVELS: usize> {
    len: usize,
    entries: [(PriceTicks, usize); LEVELS],
}

impl<const LEVELS: usize> LevelIndex<LEVELS> {
    const fn new() -> Self {
        Self {
            len: 0,
            entries: [(PriceTicks(0), 0); LEVELS],
        }
    }

    fn is_full(&self) -> bool {
        self.len == LEVELS
    }

    /// Insert a level in sorted position. Bids are sorted descending by price;
    /// asks are sorted ascending by price.
    fn insert(&mut self, price: PriceTicks, level_index: usize, descending: bool) {
        debug_assert!(!self.is_full());
        let pos = self
            .entries
            .iter()
            .take(self.len)
            .position(|&(p, _)| {
                if descending {
                    price.0 >= p.0
                } else {
                    price.0 <= p.0
                }
            })
            .unwrap_or(self.len);
        let mut i = self.len;
        while i > pos {
            self.entries[i] = self.entries[i - 1];
            i -= 1;
        }
        self.entries[pos] = (price, level_index);
        self.len += 1;
    }

    /// Remove a level by its array slot index.
    fn remove(&mut self, level_index: usize) {
        if let Some(pos) = self
            .entries
            .iter()
            .take(self.len)
            .position(|&(_, idx)| idx == level_index)
        {
            let mut i = pos;
            while i + 1 < self.len {
                self.entries[i] = self.entries[i + 1];
                i += 1;
            }
            self.len -= 1;
        }
    }

    /// Walk entries in sorted order (best to worst for the given side).
    fn iter(&self) -> impl Iterator<Item = &(PriceTicks, usize)> {
        self.entries.iter().take(self.len)
    }
}

/// Single-instrument, fixed-capacity price-time book with indexed order
/// lookup and sorted-level best-price discovery. Per-level FIFOs use stable
/// slot handles: insert, fill, and cancel never shift peer orders or rewrite
/// their index locations.
#[derive(Debug)]
pub struct OrderBook<const LEVELS: usize, const ORDERS_PER_LEVEL: usize> {
    instrument: InstrumentId,
    bids: [Option<PriceLevel<ORDERS_PER_LEVEL>>; LEVELS],
    asks: [Option<PriceLevel<ORDERS_PER_LEVEL>>; LEVELS],
    index: OrderIndex<LEVELS, ORDERS_PER_LEVEL>,
    bid_levels: LevelIndex<LEVELS>,
    ask_levels: LevelIndex<LEVELS>,
}

impl<const LEVELS: usize, const ORDERS_PER_LEVEL: usize> OrderBook<LEVELS, ORDERS_PER_LEVEL> {
    #[must_use]
    pub const fn new(instrument: InstrumentId) -> Self {
        Self {
            instrument,
            bids: [None; LEVELS],
            asks: [None; LEVELS],
            index: OrderIndex::new(),
            bid_levels: LevelIndex::new(),
            ask_levels: LevelIndex::new(),
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
            let (maker_side, head_slot, mut maker) = self
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
                    slot: head_slot,
                };
                let removed_location = self.remove_index_entry(maker.index_slot, maker.id);
                debug_assert_eq!(removed_location, Some(location));
                let levels = match maker_side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                let Some(level) = levels[level_index].as_mut() else {
                    return Err(RejectReason::ArithmeticOverflow);
                };
                let removed = level
                    .unlink(head_slot)
                    .ok_or(RejectReason::ArithmeticOverflow)?;
                debug_assert_eq!(removed.id, maker.id);
                if level.len == 0 {
                    levels[level_index] = None;
                    let level_indices = match maker_side {
                        Side::Buy => &mut self.bid_levels,
                        Side::Sell => &mut self.ask_levels,
                    };
                    level_indices.remove(level_index);
                }
            } else {
                let levels = match maker_side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                let Some(level) = levels[level_index].as_mut() else {
                    return Err(RejectReason::ArithmeticOverflow);
                };
                let Some(head) = level.front_mut() else {
                    return Err(RejectReason::ArithmeticOverflow);
                };
                head.quantity = maker.quantity;
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
        let (side, level_index, slot, owner, quantity, index_slot) = self
            .find_order(cancel.order_id)
            .ok_or(RejectReason::UnknownOrder)?;
        if owner != cancel.account_id {
            return Err(RejectReason::NotOrderOwner);
        }
        let location = OrderLocation {
            side,
            level_index,
            slot,
        };
        let removed_location = self.remove_index_entry(index_slot, cancel.order_id);
        if removed_location != Some(location) {
            return Err(RejectReason::ArithmeticOverflow);
        }
        let levels = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let level = levels[level_index]
            .as_mut()
            .ok_or(RejectReason::UnknownOrder)?;
        let removed = level.unlink(slot).ok_or(RejectReason::UnknownOrder)?;
        if removed.id != cancel.order_id {
            return Err(RejectReason::ArithmeticOverflow);
        }
        if level.len == 0 {
            levels[level_index] = None;
            let level_indices = match side {
                Side::Buy => &mut self.bid_levels,
                Side::Sell => &mut self.ask_levels,
            };
            level_indices.remove(level_index);
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
        let index = match order.side {
            Side::Buy => &self.ask_levels,
            Side::Sell => &self.bid_levels,
        };
        let mut remaining = order.quantity.0;
        let mut reports = 0_usize;
        let mut boundary: Option<PriceTicks> = None;
        for &(_, level_index) in index.iter() {
            if remaining == 0 {
                break;
            }
            let Some(level) = levels[level_index].as_ref() else {
                continue;
            };
            let crosses = match order.side {
                Side::Buy => {
                    level.price.0 <= order.price.0
                        && boundary.is_none_or(|price| level.price.0 > price.0)
                }
                Side::Sell => {
                    level.price.0 >= order.price.0
                        && boundary.is_none_or(|price| level.price.0 < price.0)
                }
            };
            if !crosses {
                continue;
            }
            boundary = Some(level.price);
            let mut cursor = level.head;
            while cursor != NIL && remaining > 0 {
                let Some(OrderSlot::Live(maker)) = level.slots.get(cursor) else {
                    break;
                };
                remaining -= remaining.min(maker.quantity.0);
                reports = reports
                    .checked_add(1)
                    .ok_or(RejectReason::ArithmeticOverflow)?;
                cursor = maker.next;
            }
        }
        Ok((remaining, reports))
    }

    fn best_crossing_level(&self, side: Side, price: PriceTicks) -> Option<usize> {
        let levels = match side {
            Side::Buy => &self.asks,
            Side::Sell => &self.bids,
        };
        let index = match side {
            Side::Buy => &self.ask_levels,
            Side::Sell => &self.bid_levels,
        };
        for &(_, level_index) in index.iter() {
            if let Some(level) = levels[level_index].as_ref() {
                let crosses = match side {
                    Side::Buy => level.price.0 <= price.0,
                    Side::Sell => level.price.0 >= price.0,
                };
                if crosses {
                    return Some(level_index);
                }
            }
        }
        None
    }

    fn front_maker(
        &self,
        taker_side: Side,
        level_index: usize,
    ) -> Option<(Side, usize, RestingOrder)> {
        let (maker_side, levels) = match taker_side {
            Side::Buy => (Side::Sell, &self.asks),
            Side::Sell => (Side::Buy, &self.bids),
        };
        let (head_slot, maker) = levels.get(level_index)?.as_ref()?.front()?;
        Some((maker_side, head_slot, maker))
    }

    fn rest(&mut self, order: NewOrder, quantity: Quantity) -> Result<(), RejectReason> {
        let (levels, index) = match order.side {
            Side::Buy => (&mut self.bids, &mut self.index),
            Side::Sell => (&mut self.asks, &mut self.index),
        };
        let level_indices = match order.side {
            Side::Buy => &mut self.bid_levels,
            Side::Sell => &mut self.ask_levels,
        };
        let descending = order.side == Side::Buy;
        let resting = RestingOrder {
            id: order.order_id,
            account_id: order.account_id,
            index_slot: 0,
            price: order.price,
            quantity,
            sequence: order.sequence,
            prev: NIL,
            next: NIL,
        };
        if let Some(level_index) = levels
            .iter()
            .position(|level| level.is_some_and(|level| level.price == order.price))
        {
            let level = levels[level_index]
                .as_mut()
                .ok_or(RejectReason::PriceLevelCapacity)?;
            let slot = level.push_tail(resting)?;
            let location = OrderLocation {
                side: order.side,
                level_index,
                slot,
            };
            let index_slot = match index.insert(order.order_id, location) {
                Ok(index_slot) => index_slot,
                Err(error) => {
                    let _ = level.unlink(slot);
                    return Err(error);
                }
            };
            let Some(stored) = level.get_live_mut(slot) else {
                let _ = level.unlink(slot);
                return Err(RejectReason::ArithmeticOverflow);
            };
            stored.index_slot = index_slot;
            return Ok(());
        }
        let level_index = levels
            .iter()
            .position(Option::is_none)
            .ok_or(RejectReason::PriceLevelCapacity)?;
        let mut level = PriceLevel::new(order.price);
        let slot = level.push_tail(resting)?;
        levels[level_index] = Some(level);
        level_indices.insert(order.price, level_index, descending);
        let location = OrderLocation {
            side: order.side,
            level_index,
            slot,
        };
        let index_slot = match index.insert(order.order_id, location) {
            Ok(index_slot) => index_slot,
            Err(error) => {
                levels[level_index] = None;
                level_indices.remove(level_index);
                return Err(error);
            }
        };
        let Some(stored) = levels[level_index]
            .as_mut()
            .and_then(|level| level.get_live_mut(slot))
        else {
            levels[level_index] = None;
            level_indices.remove(level_index);
            return Err(RejectReason::ArithmeticOverflow);
        };
        stored.index_slot = index_slot;
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
                .and_then(|level| level.get_live_mut(location.slot))
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
            .get_live(location.slot)?;
        if order.id != order_id {
            return None;
        }
        Some((
            location.side,
            location.level_index,
            location.slot,
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
        let index = match side {
            Side::Buy => &self.bid_levels,
            Side::Sell => &self.ask_levels,
        };
        for &(_, level_index) in index.iter() {
            if let Some(level) = levels[level_index].as_ref() {
                mix(digest, u64::from_be_bytes(level.price.0.to_be_bytes()));
                level.for_each_live(|order| {
                    mix(digest, order.id.0);
                    mix(digest, u64::from(order.account_id.0));
                    mix(digest, order.quantity.0);
                    mix(digest, order.sequence.0);
                });
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
                let mut seen = 0_usize;
                let mut prev = NIL;
                let mut cursor = level.head;
                while cursor != NIL {
                    let OrderSlot::Live(resting) =
                        level.slots.get(cursor).expect("chain slot in range")
                    else {
                        panic!("live chain reached a free slot");
                    };
                    assert_eq!(resting.prev, prev, "prev link matches walk");
                    let location = OrderLocation {
                        side,
                        level_index,
                        slot: cursor,
                    };
                    assert_eq!(book.index.location(resting.id), Some(location));
                    assert_eq!(
                        book.index
                            .slot(usize::try_from(resting.index_slot).expect("valid index slot")),
                        Some(&IndexSlot::Occupied {
                            order_id: resting.id,
                            location,
                        })
                    );
                    prev = cursor;
                    cursor = resting.next;
                    seen += 1;
                }
                assert_eq!(prev, level.tail, "tail matches walk end");
                assert_eq!(seen, level.len, "live count matches len");
                let mut free_seen = 0_usize;
                let mut cursor = level.free_head;
                while cursor != NIL {
                    let OrderSlot::Free { next_free } =
                        level.slots.get(cursor).expect("free slot in range")
                    else {
                        panic!("free chain reached a live slot");
                    };
                    cursor = *next_free;
                    free_seen += 1;
                }
                assert_eq!(seen + free_seen, ORDERS, "live and free sets cover slots");
                live_orders += seen;
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
    fn stable_handles_survive_unrelated_mutation() {
        let mut book = OrderBook::<4, 4>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<4>::new();
        for resting in [
            order(1, 100, 1, Side::Sell),
            order(2, 100, 1, Side::Sell),
            order(10, 101, 1, Side::Sell),
            order(11, 101, 1, Side::Sell),
            order(20, 99, 1, Side::Buy),
        ] {
            book.submit(resting, &mut reports).expect("rest order");
            reports.clear();
        }
        let before = book.index.location(OrderId(11));
        assert!(before.is_some());

        book.cancel(CancelOrder {
            order_id: OrderId(1),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(30),
        })
        .expect("cancel level A head");
        assert_index_consistent(&book);
        let fill = book
            .submit(order(21, 100, 1, Side::Buy), &mut reports)
            .expect("cross level A tail");
        assert_eq!(fill.state, OrderState::Filled);
        reports.clear();
        assert_index_consistent(&book);
        book.submit(order(12, 102, 1, Side::Sell), &mut reports)
            .expect("open level C");
        reports.clear();
        book.cancel(CancelOrder {
            order_id: OrderId(20),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(31),
        })
        .expect("cancel bid level");
        assert_index_consistent(&book);

        assert_eq!(
            book.index.location(OrderId(11)),
            before,
            "unrelated mutations preserve the stable handle"
        );
        book.cancel(CancelOrder {
            order_id: OrderId(11),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(32),
        })
        .expect("cancel through preserved handle");
        assert_index_consistent(&book);
    }

    #[test]
    fn stale_handles_fail_closed() {
        let mut book = OrderBook::<2, 4>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<4>::new();
        book.submit(order(1, 100, 1, Side::Sell), &mut reports)
            .expect("rest order");
        reports.clear();
        let cancel = |sequence: u64| CancelOrder {
            order_id: OrderId(1),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(sequence),
        };

        // Corrupt the recorded location so it points at a free slot.
        let location = book.index.location(OrderId(1)).expect("indexed order");
        let flat = book.index.find_slot(OrderId(1)).expect("index slot");
        let stale = OrderLocation {
            slot: location.slot + 1,
            ..location
        };
        *book.index.slot_mut(flat).expect("index slot") = IndexSlot::Occupied {
            order_id: OrderId(1),
            location: stale,
        };
        assert_eq!(book.find_order(OrderId(1)), None);
        assert_eq!(book.cancel(cancel(2)), Err(RejectReason::UnknownOrder));
        let level = book.asks[location.level_index].as_mut().expect("level");
        assert_eq!(level.unlink(stale.slot), None);
        assert_eq!(level.len, 1, "failed unlink left the level untouched");

        // A legitimate removal invalidates the handle: a repeat cancel fails.
        *book.index.slot_mut(flat).expect("index slot") = IndexSlot::Occupied {
            order_id: OrderId(1),
            location,
        };
        book.cancel(cancel(3)).expect("first cancel succeeds");
        assert_eq!(book.cancel(cancel(4)), Err(RejectReason::UnknownOrder));
        assert_index_consistent(&book);
    }

    #[test]
    fn full_level_rejects_atomically() {
        let mut book = OrderBook::<2, 2>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<4>::new();
        book.submit(order(1, 100, 1, Side::Sell), &mut reports)
            .expect("first order");
        reports.clear();
        book.submit(order(2, 100, 1, Side::Sell), &mut reports)
            .expect("second order");
        reports.clear();
        let digest = book.stable_digest();
        assert_eq!(
            book.submit(order(3, 100, 1, Side::Sell), &mut reports),
            Err(RejectReason::PriceLevelOrderCapacity)
        );
        assert_eq!(book.stable_digest(), digest);
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

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ModelOrder {
        id: u64,
        account: u32,
        quantity: u64,
        sequence: u64,
    }

    /// Reference array model: per-level dense FIFO with shift removal.
    #[derive(Default)]
    struct ModelBook {
        bids: std::vec::Vec<(i64, std::collections::VecDeque<ModelOrder>)>,
        asks: std::vec::Vec<(i64, std::collections::VecDeque<ModelOrder>)>,
    }

    type ModelLevels = std::vec::Vec<(i64, std::collections::VecDeque<ModelOrder>)>;

    impl ModelBook {
        /// Validation and preflight; mirrors the book's atomic rejection.
        fn plan(
            &self,
            order: &NewOrder,
            report_capacity: usize,
            level_cap: usize,
            order_cap: usize,
        ) -> Result<std::vec::Vec<i64>, RejectReason> {
            if order.price.0 <= 0 {
                return Err(RejectReason::InvalidPrice);
            }
            if order.quantity.0 == 0 {
                return Err(RejectReason::InvalidQuantity);
            }
            if [&self.bids, &self.asks]
                .into_iter()
                .flatten()
                .flat_map(|(_, queue)| queue)
                .any(|resting| resting.id == order.order_id.0)
            {
                return Err(RejectReason::DuplicateOrderId);
            }
            let (maker_levels, own_levels) = match order.side {
                Side::Buy => (&self.asks, &self.bids),
                Side::Sell => (&self.bids, &self.asks),
            };
            let mut crossing: std::vec::Vec<i64> = maker_levels
                .iter()
                .map(|(price, _)| *price)
                .filter(|price| match order.side {
                    Side::Buy => *price <= order.price.0,
                    Side::Sell => *price >= order.price.0,
                })
                .collect();
            crossing.sort_unstable();
            if order.side == Side::Sell {
                crossing.reverse();
            }
            let mut remaining = order.quantity.0;
            let mut report_count = 0_usize;
            'simulate: for price in &crossing {
                let (_, queue) = maker_levels
                    .iter()
                    .find(|(level_price, _)| level_price == price)
                    .expect("crossing level");
                for maker in queue {
                    if remaining == 0 {
                        break 'simulate;
                    }
                    remaining -= remaining.min(maker.quantity);
                    report_count += 1;
                }
            }
            if report_count > report_capacity {
                return Err(RejectReason::ReportCapacity);
            }
            if remaining > 0 {
                if let Some((_, queue)) =
                    own_levels.iter().find(|(price, _)| *price == order.price.0)
                {
                    if queue.len() == order_cap {
                        return Err(RejectReason::PriceLevelOrderCapacity);
                    }
                } else if own_levels.len() == level_cap {
                    return Err(RejectReason::PriceLevelCapacity);
                }
            }
            Ok(crossing)
        }

        fn submit(
            &mut self,
            order: &NewOrder,
            report_capacity: usize,
            level_cap: usize,
            order_cap: usize,
        ) -> Result<(OrderState, Quantity, Quantity, std::vec::Vec<OrderId>), RejectReason>
        {
            let crossing = self.plan(order, report_capacity, level_cap, order_cap)?;
            let mut remaining = order.quantity.0;
            let maker_levels = match order.side {
                Side::Buy => &mut self.asks,
                Side::Sell => &mut self.bids,
            };
            let mut makers = std::vec::Vec::new();
            for price in &crossing {
                if remaining == 0 {
                    break;
                }
                let position = maker_levels
                    .iter()
                    .position(|(level_price, _)| level_price == price)
                    .expect("crossing level");
                let queue = &mut maker_levels[position].1;
                while remaining > 0 {
                    let Some(front) = queue.front_mut() else {
                        break;
                    };
                    let traded = remaining.min(front.quantity);
                    makers.push(OrderId(front.id));
                    remaining -= traded;
                    front.quantity -= traded;
                    if front.quantity == 0 {
                        queue.pop_front();
                    }
                }
                if queue.is_empty() {
                    maker_levels.remove(position);
                }
            }
            if remaining > 0 {
                let own_levels = match order.side {
                    Side::Buy => &mut self.bids,
                    Side::Sell => &mut self.asks,
                };
                let resting = ModelOrder {
                    id: order.order_id.0,
                    account: order.account_id.0,
                    quantity: remaining,
                    sequence: order.sequence.0,
                };
                match own_levels
                    .iter_mut()
                    .find(|(price, _)| *price == order.price.0)
                {
                    Some((_, queue)) => queue.push_back(resting),
                    None => own_levels
                        .push((order.price.0, std::collections::VecDeque::from([resting]))),
                }
            }
            let filled = order.quantity.0 - remaining;
            let state = if remaining == 0 {
                OrderState::Filled
            } else if filled == 0 {
                OrderState::Accepted
            } else {
                OrderState::PartiallyFilled
            };
            Ok((state, Quantity(filled), Quantity(remaining), makers))
        }

        fn cancel(&mut self, cancel: &CancelOrder) -> Result<(u64, u32, u64), RejectReason> {
            if let Some(result) = Self::cancel_from(&mut self.bids, cancel) {
                return result;
            }
            if let Some(result) = Self::cancel_from(&mut self.asks, cancel) {
                return result;
            }
            Err(RejectReason::UnknownOrder)
        }

        fn cancel_from(
            levels: &mut ModelLevels,
            cancel: &CancelOrder,
        ) -> Option<Result<(u64, u32, u64), RejectReason>> {
            for position in 0..levels.len() {
                let queue = &mut levels[position].1;
                let Some(index) = queue.iter().position(|order| order.id == cancel.order_id.0)
                else {
                    continue;
                };
                let order = queue[index];
                if order.account != cancel.account_id.0 {
                    return Some(Err(RejectReason::NotOrderOwner));
                }
                queue.remove(index);
                if queue.is_empty() {
                    levels.remove(position);
                }
                return Some(Ok((order.id, order.account, order.quantity)));
            }
            None
        }
    }

    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, bound: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) % bound
        }
    }

    type Dump = std::vec::Vec<(Side, i64, u64, u64, u64)>;

    fn dump_levels(side: Side, levels: &ModelLevels) -> Dump {
        let mut dump = std::vec::Vec::new();
        let mut prices: std::vec::Vec<i64> = levels.iter().map(|(price, _)| *price).collect();
        prices.sort_unstable();
        for price in prices {
            let (_, queue) = levels
                .iter()
                .find(|(level_price, _)| *level_price == price)
                .expect("dump level");
            for order in queue {
                dump.push((side, price, order.id, order.quantity, order.sequence));
            }
        }
        dump
    }

    fn dump_model(model: &ModelBook) -> Dump {
        let mut dump = dump_levels(Side::Buy, &model.bids);
        dump.extend(dump_levels(Side::Sell, &model.asks));
        dump
    }

    fn dump_book<const LEVELS: usize, const ORDERS: usize>(
        book: &OrderBook<LEVELS, ORDERS>,
    ) -> Dump {
        let mut dump = std::vec::Vec::new();
        for (side, levels) in [(Side::Buy, &book.bids), (Side::Sell, &book.asks)] {
            let mut prices: std::vec::Vec<i64> =
                levels.iter().flatten().map(|level| level.price.0).collect();
            prices.sort_unstable();
            for price in prices {
                let level = levels
                    .iter()
                    .flatten()
                    .find(|level| level.price.0 == price)
                    .expect("dump level");
                level.for_each_live(|order| {
                    dump.push((side, price, order.id.0, order.quantity.0, order.sequence.0));
                });
            }
        }
        dump
    }

    fn assert_level_index_consistent<const LEVELS: usize, const ORDERS: usize>(
        book: &OrderBook<LEVELS, ORDERS>,
    ) {
        // Bids sorted descending by price.
        for window in book.bid_levels.entries[..book.bid_levels.len].windows(2) {
            assert!(
                window[0].0.0 >= window[1].0.0,
                "bid index not sorted descending: {:?} vs {:?}",
                window[0].0,
                window[1].0
            );
        }
        // Asks sorted ascending by price.
        for window in book.ask_levels.entries[..book.ask_levels.len].windows(2) {
            assert!(
                window[0].0.0 <= window[1].0.0,
                "ask index not sorted ascending: {:?} vs {:?}",
                window[0].0,
                window[1].0
            );
        }
        // Every indexed level is occupied and every occupied level is indexed.
        let mut bid_indexed = std::vec::Vec::new();
        for &(_, idx) in book.bid_levels.iter() {
            assert!(
                book.bids[idx].is_some(),
                "bid level index points to empty slot {idx}"
            );
            bid_indexed.push(idx);
        }
        for (i, level) in book.bids.iter().enumerate() {
            if level.is_some() {
                assert!(
                    bid_indexed.contains(&i),
                    "occupied bid level {i} not in index"
                );
            }
        }
        let mut ask_indexed = std::vec::Vec::new();
        for &(_, idx) in book.ask_levels.iter() {
            assert!(
                book.asks[idx].is_some(),
                "ask level index points to empty slot {idx}"
            );
            ask_indexed.push(idx);
        }
        for (i, level) in book.asks.iter().enumerate() {
            if level.is_some() {
                assert!(
                    ask_indexed.contains(&i),
                    "occupied ask level {i} not in index"
                );
            }
        }
    }

    #[test]
    fn best_bid_ask_match_sorted_index() {
        let mut book = OrderBook::<8, 4>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<8>::new();
        for (id, price, side) in [
            (1, 100, Side::Sell),
            (2, 105, Side::Sell),
            (3, 99, Side::Buy),
            (4, 95, Side::Buy),
            (5, 110, Side::Sell),
            (6, 90, Side::Buy),
        ] {
            book.submit(order(id, price, 1, side), &mut reports)
                .expect("rest order");
            reports.clear();
        }
        assert_level_index_consistent(&book);
        // Best bid should be 99 (highest buy).
        assert_eq!(
            book.best_crossing_level(Side::Buy, PriceTicks(200)),
            Some(0),
            "best bid"
        );
        // Best ask should be 100 (lowest sell).
        assert_eq!(
            book.best_crossing_level(Side::Sell, PriceTicks(50)),
            Some(0),
            "best ask"
        );
    }

    #[test]
    fn empty_levels_produce_no_crossing() {
        let book = OrderBook::<4, 2>::new(InstrumentId(1));
        assert_eq!(book.best_crossing_level(Side::Buy, PriceTicks(100)), None);
        assert_eq!(book.best_crossing_level(Side::Sell, PriceTicks(1)), None);
    }

    #[test]
    fn boundary_price_only_crosses_correct_side() {
        let mut book = OrderBook::<4, 4>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<4>::new();
        // Create separate levels so both sides persist.
        book.submit(order(1, 100, 2, Side::Sell), &mut reports)
            .expect("rest ask");
        reports.clear();
        book.submit(order(2, 99, 2, Side::Buy), &mut reports)
            .expect("rest bid");
        reports.clear();
        assert_level_index_consistent(&book);
        // Buy at exactly 100 crosses the ask at 100.
        assert_eq!(
            book.best_crossing_level(Side::Buy, PriceTicks(100)),
            Some(0)
        );
        // Sell at exactly 99 crosses the bid at 99.
        assert_eq!(
            book.best_crossing_level(Side::Sell, PriceTicks(99)),
            Some(0)
        );
        // Buy at 99 does not cross ask at 100.
        assert_eq!(book.best_crossing_level(Side::Buy, PriceTicks(99)), None);
        // Sell at 100 does not cross bid at 99.
        assert_eq!(book.best_crossing_level(Side::Sell, PriceTicks(100)), None);
    }

    #[test]
    fn level_index_survives_churn() {
        let mut book = OrderBook::<8, 4>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<16>::new();
        // Create levels at prices 98, 99, 100, 101, 102.
        for (id, price) in [(1, 98), (2, 99), (3, 100), (4, 101), (5, 102)] {
            book.submit(order(id, price, 2, Side::Sell), &mut reports)
                .expect("rest");
            reports.clear();
        }
        book.submit(order(6, 97, 2, Side::Buy), &mut reports)
            .expect("rest bid");
        reports.clear();
        assert_level_index_consistent(&book);
        // Cancel the middle ask level (100).
        book.cancel(CancelOrder {
            order_id: OrderId(3),
            account_id: AccountId(1),
            instrument_id: InstrumentId(1),
            sequence: SequenceNumber(10),
        })
        .expect("cancel middle");
        assert_level_index_consistent(&book);
        // Fill the 98 ask level completely.
        book.submit(order(7, 100, 2, Side::Buy), &mut reports)
            .expect("cross 98");
        reports.clear();
        assert_level_index_consistent(&book);
        // The best ask is now 99 (98 was fully filled, 100 was cancelled).
        let best = book.best_crossing_level(Side::Buy, PriceTicks(200));
        assert!(best.is_some(), "best ask after churn");
        let level = book.asks[best.unwrap()].as_ref().unwrap();
        assert_eq!(level.price.0, 99, "best ask price after churn");
    }

    #[test]
    fn no_skipped_liquidity() {
        let mut book = OrderBook::<8, 4>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<16>::new();
        for (id, price, qty) in [(1, 100, 5), (2, 101, 5), (3, 102, 5)] {
            book.submit(order(id, price, qty, Side::Sell), &mut reports)
                .expect("rest");
            reports.clear();
        }
        // Submit a buy that should consume all three levels in price order.
        let summary = book
            .submit(order(4, 200, 15, Side::Buy), &mut reports)
            .expect("cross all");
        assert_eq!(summary.filled_quantity, Quantity(15));
        let makers: std::vec::Vec<_> = reports.iter().map(|r| r.maker_order_id.0).collect();
        assert_eq!(makers, [1, 2, 3], "must consume levels in price order");
    }

    #[test]
    fn model_equivalent_digest_after_indexed_discovery() {
        // Build two books with the same orders; digest must match.
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
            assert_level_index_consistent(book);
        }
        assert_eq!(first.stable_digest(), second.stable_digest());
    }

    #[test]
    fn generated_commands_match_array_model() {
        const LEVELS: usize = 4;
        const ORDERS: usize = 4;
        const REPORTS: usize = 8;
        let mut book = OrderBook::<LEVELS, ORDERS>::new(InstrumentId(1));
        let mut model = ModelBook::default();
        let mut reports = ReportBuffer::<REPORTS>::new();
        let mut lcg = Lcg(0x5eed);
        let mut next_id = 1_u64;
        for step in 0..600_u64 {
            if next_id == 1 || lcg.below(5) < 3 {
                let id = next_id;
                next_id += 1;
                let side = if lcg.below(2) == 0 {
                    Side::Buy
                } else {
                    Side::Sell
                };
                let price = 99 + i64::try_from(lcg.below(3)).expect("small price");
                let command = NewOrder {
                    order_id: OrderId(id),
                    account_id: AccountId(1 + u32::try_from(lcg.below(2)).expect("small account")),
                    instrument_id: InstrumentId(1),
                    price: PriceTicks(price),
                    quantity: Quantity(1 + lcg.below(3)),
                    sequence: SequenceNumber(id),
                    side,
                };
                reports.clear();
                let actual = book.submit(command, &mut reports);
                let expected = model.submit(&command, REPORTS, LEVELS, ORDERS);
                match (actual, expected) {
                    (Ok(summary), Ok((state, filled, resting, makers))) => {
                        assert_eq!(summary.state, state, "state at step {step}");
                        assert_eq!(summary.filled_quantity, filled, "filled at step {step}");
                        assert_eq!(summary.resting_quantity, resting, "resting at step {step}");
                        let actual_makers: std::vec::Vec<_> =
                            reports.iter().map(|report| report.maker_order_id).collect();
                        assert_eq!(actual_makers, makers, "maker sequence at step {step}");
                    }
                    (Err(actual_error), Err(expected_error)) => {
                        assert_eq!(
                            actual_error, expected_error,
                            "submit rejection at step {step}"
                        );
                    }
                    (actual, expected) => {
                        panic!("submit divergence at step {step}: {actual:?} vs {expected:?}");
                    }
                }
            } else {
                let command = CancelOrder {
                    order_id: OrderId(1 + lcg.below(next_id - 1)),
                    account_id: AccountId(1 + u32::try_from(lcg.below(3)).expect("small account")),
                    instrument_id: InstrumentId(1),
                    sequence: SequenceNumber(next_id),
                };
                let actual = book.cancel(command);
                let expected = model.cancel(&command);
                match (actual, expected) {
                    (Ok(cancelled), Ok((id, account, quantity))) => {
                        assert_eq!(cancelled.order_id.0, id, "cancel id at step {step}");
                        assert_eq!(
                            cancelled.account_id.0, account,
                            "cancel account at step {step}"
                        );
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
            assert_eq!(
                dump_book(&book),
                dump_model(&model),
                "book state at step {step}"
            );
            assert_index_consistent(&book);
        }
    }
}
