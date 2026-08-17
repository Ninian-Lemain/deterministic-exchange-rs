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

/// Open-addressed `OrderId -> OrderLocation` index with linear probing and
/// deterministic back-shift deletion. Both sides share one index: four planes
/// keep the live load at or below 50% of slots (two sides of at most
/// `LEVELS * ORDERS` orders each), so probes stay short at maximum occupancy.
/// The planes exist as a nested array because stable Rust cannot express
/// `LEVELS * ORDERS * PLANES` as a single array length over const generics.
#[derive(Debug)]
struct OrderIndex<const LEVELS: usize, const ORDERS: usize> {
    slots: [[[IndexSlot; ORDERS]; LEVELS]; ORDER_INDEX_PLANES],
}

impl<const LEVELS: usize, const ORDERS: usize> OrderIndex<LEVELS, ORDERS> {
    const CAPACITY: usize = LEVELS * ORDERS * ORDER_INDEX_PLANES;

    const fn new() -> Self {
        Self {
            slots: [[[IndexSlot::Empty; ORDERS]; LEVELS]; ORDER_INDEX_PLANES],
        }
    }

    /// Flat-index coordinates. Callers only pass indices below `CAPACITY`.
    fn coordinates(flat_index: usize) -> (usize, usize, usize) {
        debug_assert!(flat_index < Self::CAPACITY);
        let per_plane = LEVELS * ORDERS;
        let plane = flat_index / per_plane;
        let within_plane = flat_index % per_plane;
        (plane, within_plane / ORDERS, within_plane % ORDERS)
    }

    fn slot(&self, flat_index: usize) -> &IndexSlot {
        let (plane, level, order) = Self::coordinates(flat_index);
        &self.slots[plane][level][order]
    }

    fn slot_mut(&mut self, flat_index: usize) -> &mut IndexSlot {
        let (plane, level, order) = Self::coordinates(flat_index);
        &mut self.slots[plane][level][order]
    }

    /// Probing requires a non-zero capacity; callers guard `CAPACITY == 0`.
    fn probe_start(order_id: OrderId) -> usize {
        let capacity = u64::try_from(Self::CAPACITY).unwrap_or(u64::MAX);
        let mixed = order_id.0.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        usize::try_from(mixed % capacity).unwrap_or(0)
    }

    fn probe_distance(home: usize, current: usize) -> usize {
        if current >= home {
            current - home
        } else {
            Self::CAPACITY - (home - current)
        }
    }

    fn find_slot(&self, order_id: OrderId) -> Option<usize> {
        if Self::CAPACITY == 0 {
            return None;
        }
        let start = Self::probe_start(order_id);
        for offset in 0..Self::CAPACITY {
            let flat_index = start.wrapping_add(offset) % Self::CAPACITY;
            match self.slot(flat_index) {
                IndexSlot::Empty => return None,
                IndexSlot::Occupied {
                    order_id: indexed, ..
                } if *indexed == order_id => return Some(flat_index),
                IndexSlot::Occupied { .. } => {}
            }
        }
        None
    }

    fn location(&self, order_id: OrderId) -> Option<OrderLocation> {
        match self.slot(self.find_slot(order_id)?) {
            IndexSlot::Occupied { location, .. } => Some(*location),
            IndexSlot::Empty => None,
        }
    }

    fn insert(&mut self, order_id: OrderId, location: OrderLocation) -> Result<u32, RejectReason> {
        if Self::CAPACITY == 0 {
            return Err(RejectReason::OrderCapacity);
        }
        let start = Self::probe_start(order_id);
        for offset in 0..Self::CAPACITY {
            let flat_index = start.wrapping_add(offset) % Self::CAPACITY;
            match self.slot(flat_index) {
                IndexSlot::Empty => {
                    let slot_id =
                        u32::try_from(flat_index).map_err(|_| RejectReason::OrderCapacity)?;
                    *self.slot_mut(flat_index) = IndexSlot::Occupied { order_id, location };
                    return Ok(slot_id);
                }
                IndexSlot::Occupied {
                    order_id: indexed, ..
                } if *indexed == order_id => return Err(RejectReason::DuplicateOrderId),
                IndexSlot::Occupied { .. } => {}
            }
        }
        Err(RejectReason::OrderCapacity)
    }

    /// Removes `order_id` at `slot_index` and closes the probe hole by
    /// shifting displaced entries back, reporting each move through
    /// `update_reverse_slot`. Fails closed on a stale handle.
    fn remove_at(
        &mut self,
        slot_index: u32,
        order_id: OrderId,
        mut update_reverse_slot: impl FnMut(OrderId, OrderLocation, u32) -> bool,
    ) -> Option<OrderLocation> {
        let flat_index = usize::try_from(slot_index).ok()?;
        if flat_index >= Self::CAPACITY {
            return None;
        }
        let IndexSlot::Occupied {
            order_id: indexed,
            location,
        } = *self.slot(flat_index)
        else {
            return None;
        };
        if indexed != order_id {
            return None;
        }
        let mut hole = flat_index;
        let mut candidate_index = hole.wrapping_add(1) % Self::CAPACITY;
        // Close the probe hole without retaining deletion tombstones.
        loop {
            let candidate = *self.slot(candidate_index);
            let IndexSlot::Occupied {
                order_id: candidate_id,
                location: candidate_location,
            } = candidate
            else {
                *self.slot_mut(hole) = IndexSlot::Empty;
                break;
            };
            let home_bucket = Self::probe_start(candidate_id);
            if Self::probe_distance(home_bucket, hole)
                < Self::probe_distance(home_bucket, candidate_index)
            {
                *self.slot_mut(hole) = candidate;
                let new_slot = u32::try_from(hole).expect("occupied slots fit u32");
                debug_assert!(update_reverse_slot(
                    candidate_id,
                    candidate_location,
                    new_slot
                ));
                hole = candidate_index;
            }
            candidate_index = candidate_index.wrapping_add(1) % Self::CAPACITY;
        }
        Some(location)
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

    /// Appends at the tail and returns the stable slot handle, or `None` when
    /// the level is full or its links are corrupt. Nothing mutates on `None`.
    fn push_tail(&mut self, mut order: RestingOrder) -> Option<usize> {
        if self.free_head == NIL || self.len >= ORDERS {
            return None;
        }
        let slot = self.free_head;
        let Some(OrderSlot::Free { next_free }) = self.slots.get(slot).copied() else {
            return None;
        };
        if self.tail != NIL {
            match self.slots.get_mut(self.tail) {
                Some(OrderSlot::Live(tail_order)) => tail_order.next = slot,
                _ => return None,
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
        Some(slot)
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

/// Sorted index of occupied level slots plus the pool of free level slots for
/// one side. Bids sort by price descending (best bid first); asks sort
/// ascending (best ask first). Price lookup is O(log n) by binary search,
/// best-price discovery is the first entry, and slot allocation is O(1) from
/// the free pool. Occupied and free slots partition `0..LEVELS` exactly.
#[derive(Debug)]
struct LevelIndex<const LEVELS: usize> {
    len: usize,
    entries: [(PriceTicks, usize); LEVELS],
    free: [usize; LEVELS],
    free_len: usize,
}

impl<const LEVELS: usize> LevelIndex<LEVELS> {
    const fn new() -> Self {
        let mut free = [0_usize; LEVELS];
        let mut index = 0;
        while index < LEVELS {
            // The pool pops from the end, so a fresh book allocates the lowest
            // level slots first.
            free[index] = LEVELS - 1 - index;
            index += 1;
        }
        Self {
            len: 0,
            entries: [(PriceTicks(0), 0); LEVELS],
            free,
            free_len: LEVELS,
        }
    }

    fn is_full(&self) -> bool {
        self.free_len == 0
    }

    /// Sorted position of `price`: the insertion point and whether an entry
    /// with that exact price occupies it.
    fn position(&self, price: PriceTicks, descending: bool) -> (usize, bool) {
        let entries = &self.entries[..self.len];
        let position = entries.partition_point(|&(listed, _)| {
            if descending {
                listed.0 > price.0
            } else {
                listed.0 < price.0
            }
        });
        let found = position < self.len && entries[position].0 == price;
        (position, found)
    }

    /// Level slot holding `price`, if one is occupied at that price.
    fn find(&self, price: PriceTicks, descending: bool) -> Option<usize> {
        let (position, found) = self.position(price, descending);
        if found {
            Some(self.entries[position].1)
        } else {
            None
        }
    }

    /// Allocates a free level slot and indexes it at `price` in sorted order.
    /// Returns the slot, or `None` when every level slot is occupied.
    fn insert(&mut self, price: PriceTicks, descending: bool) -> Option<usize> {
        let (position, found) = self.position(price, descending);
        debug_assert!(!found, "price levels are unique per side");
        if self.free_len == 0 {
            return None;
        }
        self.free_len -= 1;
        let level_index = self.free[self.free_len];
        self.entries.copy_within(position..self.len, position + 1);
        self.entries[position] = (price, level_index);
        self.len += 1;
        Some(level_index)
    }

    /// Removes the level at `price` and returns its slot to the free pool.
    fn remove(&mut self, price: PriceTicks, descending: bool) -> Option<usize> {
        let (position, found) = self.position(price, descending);
        if !found {
            return None;
        }
        let level_index = self.entries[position].1;
        self.entries.copy_within(position + 1..self.len, position);
        self.len -= 1;
        self.free[self.free_len] = level_index;
        self.free_len += 1;
        Some(level_index)
    }

    /// Walk entries in sorted order (best to worst for the given side).
    fn iter(&self) -> impl Iterator<Item = &(PriceTicks, usize)> {
        self.entries.iter().take(self.len)
    }
}

/// A single fill entry in a match plan. Records the maker order location and
/// the traded quantity so that `apply_plan` can execute the fill without
/// re-walking the book.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FillEntry {
    level_index: usize,
    slot: usize,
    order_id: OrderId,
    price: PriceTicks,
    quantity: Quantity,
}

/// Compact match plan with bounded fill entries. Built during a single walk
/// of the crossing levels; applied by direct mutation using the captured
/// locations. A plan never heap-allocates and is bounded by the report
/// capacity available at build time.
#[derive(Clone, Debug)]
struct MatchPlan<const FILLS: usize> {
    fills: [FillEntry; FILLS],
    fill_count: usize,
    capacity: usize,
    maker_side: Side,
    resting_quantity: Quantity,
}

const DUMMY_FILL: FillEntry = FillEntry {
    level_index: 0,
    slot: 0,
    order_id: OrderId(0),
    price: PriceTicks(0),
    quantity: Quantity(0),
};

impl<const FILLS: usize> MatchPlan<FILLS> {
    fn new(maker_side: Side, report_capacity: usize) -> Self {
        Self {
            fills: [DUMMY_FILL; FILLS],
            fill_count: 0,
            capacity: report_capacity.min(FILLS),
            maker_side,
            resting_quantity: Quantity(0),
        }
    }

    fn push_fill(
        &mut self,
        level_index: usize,
        slot: usize,
        order_id: OrderId,
        price: PriceTicks,
        quantity: Quantity,
    ) -> Result<(), RejectReason> {
        if self.fill_count >= self.capacity {
            return Err(RejectReason::ReportCapacity);
        }
        self.fills[self.fill_count] = FillEntry {
            level_index,
            slot,
            order_id,
            price,
            quantity,
        };
        self.fill_count += 1;
        Ok(())
    }

    #[must_use]
    fn fills(&self) -> &[FillEntry] {
        &self.fills[..self.fill_count]
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

    fn side_levels(&self, side: Side) -> &[Option<PriceLevel<ORDERS_PER_LEVEL>>; LEVELS] {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }

    fn side_levels_mut(
        &mut self,
        side: Side,
    ) -> &mut [Option<PriceLevel<ORDERS_PER_LEVEL>>; LEVELS] {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }

    fn side_index(&self, side: Side) -> &LevelIndex<LEVELS> {
        match side {
            Side::Buy => &self.bid_levels,
            Side::Sell => &self.ask_levels,
        }
    }

    fn side_index_mut(&mut self, side: Side) -> &mut LevelIndex<LEVELS> {
        match side {
            Side::Buy => &mut self.bid_levels,
            Side::Sell => &mut self.ask_levels,
        }
    }

    /// # Errors
    ///
    /// Returns an explicit validation or capacity rejection. `build_plan`
    /// preflights every fallible condition, so a rejection provably leaves the
    /// book and report buffer untouched.
    pub fn submit<const REPORTS: usize>(
        &mut self,
        order: NewOrder,
        reports: &mut ReportBuffer<REPORTS>,
    ) -> Result<MatchSummary, RejectReason> {
        let initial_report_count = reports.len();
        let plan = self.build_plan::<REPORTS>(order, reports.remaining_capacity())?;
        self.apply_plan(order, &plan, reports);
        let filled = order.quantity.0 - plan.resting_quantity.0;
        let state = if plan.resting_quantity.0 == 0 {
            OrderState::Filled
        } else if filled == 0 {
            OrderState::Accepted
        } else {
            OrderState::PartiallyFilled
        };
        Ok(MatchSummary {
            state,
            filled_quantity: Quantity(filled),
            resting_quantity: plan.resting_quantity,
            report_count: reports.len() - initial_report_count,
        })
    }

    /// Walks crossing levels once to build a compact match plan, preflighting
    /// validation, duplicates, report capacity, and resting capacity. Because
    /// every fallible condition is decided here against an unchanged book,
    /// `apply_plan` afterwards is infallible and needs no rollback path.
    fn build_plan<const REPORTS: usize>(
        &self,
        order: NewOrder,
        report_capacity: usize,
    ) -> Result<MatchPlan<REPORTS>, RejectReason> {
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
        let maker_side = match order.side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let maker_levels = self.side_levels(maker_side);
        let mut plan = MatchPlan::<REPORTS>::new(maker_side, report_capacity);
        let mut remaining = order.quantity.0;
        for &(price, level_index) in self.side_index(maker_side).iter() {
            if remaining == 0 {
                break;
            }
            let crosses = match order.side {
                Side::Buy => price.0 <= order.price.0,
                Side::Sell => price.0 >= order.price.0,
            };
            // Levels are visited best to worst, so the first non-crossing
            // level ends the walk.
            if !crosses {
                break;
            }
            let level = maker_levels[level_index]
                .as_ref()
                .expect("indexed level is occupied");
            let mut cursor = level.head;
            while cursor != NIL && remaining > 0 {
                let OrderSlot::Live(maker) = level.slots[cursor] else {
                    unreachable!("live chain only links live slots");
                };
                let traded = remaining.min(maker.quantity.0);
                plan.push_fill(level_index, cursor, maker.id, price, Quantity(traded))?;
                remaining -= traded;
                cursor = maker.next;
            }
        }
        if remaining > 0 {
            let descending = order.side == Side::Buy;
            if let Some(level_index) = self.side_index(order.side).find(order.price, descending) {
                let level = self.side_levels(order.side)[level_index]
                    .as_ref()
                    .expect("indexed level is occupied");
                if level.len == ORDERS_PER_LEVEL {
                    return Err(RejectReason::PriceLevelOrderCapacity);
                }
            } else if self.side_index(order.side).is_full() {
                return Err(RejectReason::PriceLevelCapacity);
            }
        }
        plan.resting_quantity = Quantity(remaining);
        Ok(plan)
    }

    /// Applies a pre-built match plan, emitting one execution report per fill.
    /// Infallible: `build_plan` preflighted report capacity, level capacity,
    /// and duplicates, and the book cannot change between the two calls.
    /// Violations of those preflighted invariants are bugs, not rejections.
    fn apply_plan<const REPORTS: usize>(
        &mut self,
        order: NewOrder,
        plan: &MatchPlan<REPORTS>,
        reports: &mut ReportBuffer<REPORTS>,
    ) {
        for fill in plan.fills() {
            let (index_slot, full_fill) = {
                let level = self.side_levels(plan.maker_side)[fill.level_index]
                    .as_ref()
                    .expect("plan level is occupied");
                let maker = level.get_live(fill.slot).expect("plan maker is live");
                debug_assert_eq!(maker.id, fill.order_id);
                reports
                    .push(ExecutionReport {
                        maker_order_id: fill.order_id,
                        taker_order_id: order.order_id,
                        instrument_id: order.instrument_id,
                        price: fill.price,
                        quantity: fill.quantity,
                        sequence: order.sequence,
                    })
                    .expect("report capacity was preflighted");
                (maker.index_slot, maker.quantity.0 == fill.quantity.0)
            };
            if full_fill {
                let removed_location = self.remove_index_entry(index_slot, fill.order_id);
                debug_assert_eq!(
                    removed_location,
                    Some(OrderLocation {
                        side: plan.maker_side,
                        level_index: fill.level_index,
                        slot: fill.slot,
                    })
                );
                let level = self.side_levels_mut(plan.maker_side)[fill.level_index]
                    .as_mut()
                    .expect("plan level is occupied");
                let removed = level.unlink(fill.slot).expect("plan maker is live");
                debug_assert_eq!(removed.quantity, fill.quantity);
                if level.len == 0 {
                    let price = level.price;
                    self.side_levels_mut(plan.maker_side)[fill.level_index] = None;
                    let removed_index = self
                        .side_index_mut(plan.maker_side)
                        .remove(price, plan.maker_side == Side::Buy);
                    debug_assert_eq!(removed_index, Some(fill.level_index));
                }
            } else {
                let maker = self.side_levels_mut(plan.maker_side)[fill.level_index]
                    .as_mut()
                    .and_then(|level| level.get_live_mut(fill.slot))
                    .expect("plan maker is live");
                maker.quantity.0 -= fill.quantity.0;
            }
        }
        if plan.resting_quantity.0 > 0 {
            self.rest(order, plan.resting_quantity);
        }
    }

    /// Removes an owned resting order while retaining FIFO order for all peers.
    ///
    /// # Errors
    ///
    /// Returns an instrument, unknown-order, or ownership rejection without
    /// mutating the book.
    ///
    /// # Panics
    ///
    /// Panics only if the book's internal index invariants are broken, which
    /// is a bug, not a rejection path.
    pub fn cancel(&mut self, cancel: CancelOrder) -> Result<CancelledOrder, RejectReason> {
        if cancel.instrument_id != self.instrument {
            return Err(RejectReason::InvalidInstrument);
        }
        let (location, owner) = self
            .locate(cancel.order_id)
            .ok_or(RejectReason::UnknownOrder)?;
        if owner != cancel.account_id {
            return Err(RejectReason::NotOrderOwner);
        }
        // Located and owner-checked: removal cannot fail.
        let level = self.side_levels_mut(location.side)[location.level_index]
            .as_mut()
            .expect("indexed level is occupied");
        let removed = level.unlink(location.slot).expect("indexed slot is live");
        debug_assert_eq!(removed.id, cancel.order_id);
        if level.len == 0 {
            let price = level.price;
            self.side_levels_mut(location.side)[location.level_index] = None;
            let removed_index = self
                .side_index_mut(location.side)
                .remove(price, location.side == Side::Buy);
            debug_assert_eq!(removed_index, Some(location.level_index));
        }
        let removed_location = self.remove_index_entry(removed.index_slot, cancel.order_id);
        debug_assert_eq!(removed_location, Some(location));
        Ok(CancelledOrder {
            order_id: removed.id,
            account_id: removed.account_id,
            quantity: removed.quantity,
        })
    }

    #[cfg(test)]
    fn best_crossing_level(&self, side: Side, price: PriceTicks) -> Option<usize> {
        let maker_side = match side {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let &(level_price, level_index) = self.side_index(maker_side).iter().next()?;
        let crosses = match side {
            Side::Buy => level_price.0 <= price.0,
            Side::Sell => level_price.0 >= price.0,
        };
        if crosses { Some(level_index) } else { None }
    }

    /// Rests the unfilled remainder. Infallible: `build_plan` preflighted a
    /// level slot with room or a free level slot, and the order index always
    /// has spare capacity (four planes for at most `2 * LEVELS * ORDERS` live
    /// orders).
    fn rest(&mut self, order: NewOrder, quantity: Quantity) {
        let Self {
            bids,
            asks,
            index,
            bid_levels,
            ask_levels,
            ..
        } = self;
        let (levels, sorted) = match order.side {
            Side::Buy => (bids, bid_levels),
            Side::Sell => (asks, ask_levels),
        };
        let descending = order.side == Side::Buy;
        let level_index = if let Some(level_index) = sorted.find(order.price, descending) {
            level_index
        } else {
            let level_index = sorted
                .insert(order.price, descending)
                .expect("level capacity was preflighted");
            levels[level_index] = Some(PriceLevel::new(order.price));
            level_index
        };
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
        let level = levels[level_index]
            .as_mut()
            .expect("indexed level is occupied");
        let slot = level
            .push_tail(resting)
            .expect("level order capacity was preflighted");
        let location = OrderLocation {
            side: order.side,
            level_index,
            slot,
        };
        let index_slot = index
            .insert(order.order_id, location)
            .expect("order index capacity was preflighted");
        level
            .get_live_mut(slot)
            .expect("slot was just inserted")
            .index_slot = index_slot;
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

    /// Resolves an order ID to its location and owner, failing closed when the
    /// index entry is stale.
    fn locate(&self, order_id: OrderId) -> Option<(OrderLocation, AccountId)> {
        let location = self.index.location(order_id)?;
        let order = self
            .side_levels(location.side)
            .get(location.level_index)?
            .as_ref()?
            .get_live(location.slot)?;
        if order.id != order_id {
            return None;
        }
        Some((location, order.account_id))
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
        let levels = self.side_levels(side);
        for &(_, level_index) in self.side_index(side).iter() {
            if let Some(level) = levels[level_index].as_ref() {
                // Bit-identical on any CPU byte order (MSRV precludes
                // `cast_unsigned`).
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
                        &IndexSlot::Occupied {
                            order_id: resting.id,
                            location,
                        }
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
        *book.index.slot_mut(flat) = IndexSlot::Occupied {
            order_id: OrderId(1),
            location: stale,
        };
        assert_eq!(book.locate(OrderId(1)), None);
        assert_eq!(book.cancel(cancel(2)), Err(RejectReason::UnknownOrder));
        let level = book.asks[location.level_index].as_mut().expect("level");
        assert_eq!(level.unlink(stale.slot), None);
        assert_eq!(level.len, 1, "failed unlink left the level untouched");

        // A legitimate removal invalidates the handle: a repeat cancel fails.
        *book.index.slot_mut(flat) = IndexSlot::Occupied {
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

    fn assert_side_index_consistent<const LEVELS: usize, const ORDERS: usize>(
        levels: &[Option<PriceLevel<ORDERS>>; LEVELS],
        index: &LevelIndex<LEVELS>,
        descending: bool,
    ) {
        // Sorted in side order with unique prices.
        for window in index.entries[..index.len].windows(2) {
            let (best, worse) = (window[0].0.0, window[1].0.0);
            if descending {
                assert!(best > worse, "bid index not strictly descending");
            } else {
                assert!(best < worse, "ask index not strictly ascending");
            }
        }
        // Occupied and free slots partition the level array exactly once.
        assert_eq!(index.len + index.free_len, LEVELS, "slot sets cover levels");
        let mut indexed = vec![false; LEVELS];
        for &(price, slot) in index.iter() {
            assert!(slot < LEVELS, "indexed slot in range");
            assert!(!indexed[slot], "slot {slot} indexed twice");
            indexed[slot] = true;
            let level = levels[slot].as_ref().expect("indexed level occupied");
            assert_eq!(level.price, price, "indexed price matches the level");
        }
        let mut freed = vec![false; LEVELS];
        for &slot in &index.free[..index.free_len] {
            assert!(slot < LEVELS, "free slot in range");
            assert!(!freed[slot], "slot {slot} freed twice");
            assert!(!indexed[slot], "slot {slot} both free and indexed");
            freed[slot] = true;
        }
        for (slot, level) in levels.iter().enumerate() {
            assert_eq!(
                level.is_some(),
                indexed[slot],
                "slot {slot} occupancy matches index"
            );
        }
    }

    fn assert_level_index_consistent<const LEVELS: usize, const ORDERS: usize>(
        book: &OrderBook<LEVELS, ORDERS>,
    ) {
        assert_side_index_consistent(&book.bids, &book.bid_levels, true);
        assert_side_index_consistent(&book.asks, &book.ask_levels, false);
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

    #[test]
    fn duplicate_resting_id_is_rejected_without_mutation() {
        let mut book = OrderBook::<2, 2>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<2>::new();
        book.submit(order(1, 100, 1, Side::Sell), &mut reports)
            .expect("rest ask");
        let digest = book.stable_digest();
        assert_eq!(
            book.submit(order(1, 101, 1, Side::Sell), &mut reports),
            Err(RejectReason::DuplicateOrderId)
        );
        assert_eq!(book.stable_digest(), digest);
        assert_index_consistent(&book);
    }

    #[test]
    fn crossing_stops_at_the_taker_price_limit() {
        let mut book = OrderBook::<8, 2>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<8>::new();
        for (id, price) in [(1, 100), (2, 105), (3, 110)] {
            book.submit(order(id, price, 2, Side::Sell), &mut reports)
                .expect("rest ask");
            reports.clear();
        }
        // A buy at 105 must not touch the 110 ask.
        let summary = book
            .submit(order(4, 105, 4, Side::Buy), &mut reports)
            .expect("cross two levels");
        assert_eq!(summary.state, OrderState::Filled);
        let makers: std::vec::Vec<_> = reports.iter().map(|report| report.maker_order_id).collect();
        assert_eq!(makers, [OrderId(1), OrderId(2)]);
        assert_eq!(book.order_count(), 1);
        assert_index_consistent(&book);
        assert_level_index_consistent(&book);
    }

    #[test]
    fn partial_cross_then_rest_keeps_price_time_position() {
        let mut book = OrderBook::<4, 4>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<4>::new();
        book.submit(order(1, 100, 5, Side::Sell), &mut reports)
            .expect("rest ask");
        reports.clear();
        // Taker fills the 5 and rests the remaining 2 as a bid at 100.
        let summary = book
            .submit(order(2, 100, 7, Side::Buy), &mut reports)
            .expect("partial cross then rest");
        assert_eq!(summary.state, OrderState::PartiallyFilled);
        assert_eq!(summary.resting_quantity, Quantity(2));
        reports.clear();
        // A second bid at the same price queues behind the rested taker.
        book.submit(order(3, 100, 1, Side::Buy), &mut reports)
            .expect("join bid level");
        reports.clear();
        // A sell at 100 fills the rested taker (2), then the queued bid (1 of
        // 1), and rests its own remainder: price-time order within the level.
        book.submit(order(4, 100, 4, Side::Sell), &mut reports)
            .expect("cross bid level");
        let makers: std::vec::Vec<_> = reports.iter().map(|report| report.maker_order_id).collect();
        assert_eq!(makers, [OrderId(2), OrderId(3)]);
        assert_index_consistent(&book);
    }

    #[test]
    fn exact_report_capacity_fit_succeeds() {
        let mut book = OrderBook::<4, 2>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<2>::new();
        book.submit(order(1, 100, 1, Side::Sell), &mut reports)
            .expect("first ask");
        reports.clear();
        book.submit(order(2, 101, 1, Side::Sell), &mut reports)
            .expect("second ask");
        reports.clear();
        let summary = book
            .submit(order(3, 101, 2, Side::Buy), &mut reports)
            .expect("both fills exactly fit the report buffer");
        assert_eq!(summary.state, OrderState::Filled);
        assert_eq!(summary.report_count, 2);
        assert_index_consistent(&book);
    }

    #[test]
    fn emptied_level_slot_is_reused_by_later_rest() {
        let mut book = OrderBook::<4, 2>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<4>::new();
        book.submit(order(1, 100, 2, Side::Sell), &mut reports)
            .expect("rest ask");
        reports.clear();
        // Fully fill the only ask level: it is removed and its slot freed.
        book.submit(order(2, 100, 2, Side::Buy), &mut reports)
            .expect("fill the level");
        reports.clear();
        assert_level_index_consistent(&book);
        // Rest at the same price again: the freed slot is reused.
        book.submit(order(3, 100, 1, Side::Sell), &mut reports)
            .expect("re-rest at the same price");
        assert_eq!(book.order_count(), 1);
        assert_index_consistent(&book);
        assert_level_index_consistent(&book);
    }

    #[test]
    fn rejected_rest_levels_and_reports_stay_untouched() {
        let mut book = OrderBook::<2, 2>::new(InstrumentId(1));
        let mut reports = ReportBuffer::<1>::new();
        for (id, price) in [(1, 101), (2, 102)] {
            book.submit(order(id, price, 1, Side::Sell), &mut reports)
                .expect("rest ask");
            reports.clear();
        }
        // Both level slots are occupied: a new resting price must reject.
        let digest = book.stable_digest();
        assert_eq!(
            book.submit(order(3, 99, 1, Side::Sell), &mut reports),
            Err(RejectReason::PriceLevelCapacity)
        );
        assert_eq!(reports.len(), 0);
        assert_eq!(book.stable_digest(), digest);
        assert_index_consistent(&book);
        assert_level_index_consistent(&book);
    }
}
