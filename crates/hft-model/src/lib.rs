//! Deterministic bounded command generation plus reference models used by
//! property tests. Off-hot-path support code; heap use is acceptable here.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, VecDeque};

use hft_types::{
    AccountId, CancelOrder, InstrumentId, NewOrder, OrderId, OrderState, PriceTicks, Quantity,
    RejectReason, ReplaceOrder, SequenceNumber, Side, TimeInForce,
};

/// Deterministic `SplitMix64` stream. Same seed, same sequence, integer math
/// only, identical on every platform.
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    #[must_use]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_6c15);
        let mut mixed = self.state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        mixed ^ (mixed >> 31)
    }

    /// Bounded draw in `0..bound` via multiply-shift.
    ///
    /// # Panics
    ///
    /// Panics when `bound` is zero.
    #[must_use]
    pub fn below(&mut self, bound: u64) -> u64 {
        assert!(bound > 0, "bound must be positive");
        let scaled = u128::from(self.next_u64()) * u128::from(bound);
        u64::try_from(scaled >> 64).expect("high word fits u64")
    }

    #[must_use]
    pub fn coin(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

/// Bounds for generated traffic. Account ids are drawn from `1..=accounts`.
#[derive(Clone, Debug)]
pub struct GenConfig {
    pub accounts: u32,
    pub minimum_price: i64,
    pub maximum_price: i64,
    pub max_quantity: u64,
    pub cancel_probability_pct: u64,
    pub duplicate_id_probability_pct: u64,
    /// Share of New commands using IOC; FOK follows with its own share.
    pub ioc_probability_pct: u64,
    pub fok_probability_pct: u64,
    /// Share of New commands flagged post-only.
    pub post_only_probability_pct: u64,
    /// Share of Cancel commands upgraded to owned replaces.
    pub replace_probability_pct: u64,
}

/// One generated command with its strict session sequence number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    New(NewOrder),
    Cancel(CancelOrder),
    Replace(ReplaceOrder),
}

/// Bounded deterministic command stream. Sequence numbers start at 1 and grow
/// by exactly one per emitted command. Fresh order ids are strictly increasing
/// from 1; with duplicate probability a New reuses a previously issued id so
/// engines must reject it. Cancels target any issued id with any configured
/// owner, exercising unknown-order and wrong-owner paths.
#[derive(Debug)]
pub struct CommandGen {
    config: GenConfig,
    instrument: InstrumentId,
    rng: Rng,
    next_sequence: u64,
    fresh_ids: u64,
}

impl CommandGen {
    /// # Panics
    ///
    /// Panics on an empty account range, an inverted price range, a zero
    /// quantity bound, or a probability above 100 percent.
    #[must_use]
    pub fn new(config: GenConfig, instrument: InstrumentId, seed: u64) -> Self {
        assert!(config.accounts >= 1, "at least one account");
        assert!(
            config.minimum_price <= config.maximum_price,
            "price range inverted"
        );
        assert!(config.max_quantity >= 1, "quantity range empty");
        assert!(config.cancel_probability_pct <= 100);
        assert!(config.duplicate_id_probability_pct <= 100);
        assert!(config.ioc_probability_pct <= 100);
        assert!(config.fok_probability_pct <= 100);
        assert!(config.post_only_probability_pct <= 100);
        assert!(
            config.ioc_probability_pct
                + config.fok_probability_pct
                + config.post_only_probability_pct
                <= 100,
            "tif probabilities exceed 100 percent"
        );
        assert!(config.replace_probability_pct <= 100);
        Self {
            config,
            instrument,
            rng: Rng::new(seed),
            next_sequence: 1,
            fresh_ids: 0,
        }
    }

    /// Next weighted command; forced New until the first fresh id exists.
    pub fn next_command(&mut self) -> Command {
        if self.fresh_ids == 0 || self.rng.below(100) >= self.config.cancel_probability_pct {
            Command::New(self.next_new())
        } else if self.rng.below(100) < self.config.replace_probability_pct {
            Command::Replace(self.next_replace())
        } else {
            Command::Cancel(self.next_cancel())
        }
    }

    /// Owned amend of a random issued id: quantity spans up to twice the
    /// configured maximum so increases and reductions both occur; the price
    /// stays inside the configured band.
    ///
    /// # Panics
    ///
    /// Panics when no order id has been issued yet.
    pub fn next_replace(&mut self) -> ReplaceOrder {
        assert!(self.fresh_ids >= 1, "no issued ids to replace");
        let sequence = self.next_sequence();
        let span = u64::try_from(
            self.config
                .maximum_price
                .checked_sub(self.config.minimum_price)
                .expect("ordered range")
                + 1,
        )
        .expect("positive span");
        let price = self
            .config
            .minimum_price
            .checked_add(i64::try_from(self.rng.below(span)).expect("small offset"))
            .expect("in-range price");
        ReplaceOrder {
            order_id: OrderId(1 + self.rng.below(self.fresh_ids)),
            account_id: self.next_account(),
            instrument_id: self.instrument,
            sequence: SequenceNumber(sequence),
            price: PriceTicks(price),
            quantity: Quantity(1 + self.rng.below(self.config.max_quantity * 2)),
        }
    }

    /// Next fresh or duplicate-id new order.
    ///
    /// # Panics
    ///
    /// Panics on a misconfigured price range (checked in `new`).
    pub fn next_new(&mut self) -> NewOrder {
        let sequence = self.next_sequence();
        let order_id = if self.fresh_ids > 0
            && self.rng.below(100) < self.config.duplicate_id_probability_pct
        {
            OrderId(1 + self.rng.below(self.fresh_ids))
        } else {
            self.fresh_ids += 1;
            OrderId(self.fresh_ids)
        };
        let span = u64::try_from(
            self.config
                .maximum_price
                .checked_sub(self.config.minimum_price)
                .expect("ordered range")
                + 1,
        )
        .expect("positive span");
        let quantity_bound = self.config.max_quantity;
        // TIF draw: FOK, then IOC, then post-only, else GTC.
        let tif_draw = self.rng.below(100);
        let ioc_start = self.config.fok_probability_pct;
        let po_start = ioc_start + self.config.ioc_probability_pct;
        let po_end = po_start + self.config.post_only_probability_pct;
        let time_in_force = if tif_draw < ioc_start {
            TimeInForce::Fok
        } else if tif_draw < po_start {
            TimeInForce::Ioc
        } else if tif_draw < po_end {
            TimeInForce::PostOnly
        } else {
            TimeInForce::Gtc
        };
        NewOrder {
            time_in_force,
            order_id,
            account_id: self.next_account(),
            instrument_id: self.instrument,
            price: PriceTicks(
                self.config
                    .minimum_price
                    .checked_add(i64::try_from(self.rng.below(span)).expect("small offset"))
                    .expect("in-range price"),
            ),
            quantity: Quantity(1 + self.rng.below(quantity_bound)),
            sequence: SequenceNumber(sequence),
            side: if self.rng.coin() {
                Side::Buy
            } else {
                Side::Sell
            },
        }
    }

    /// # Panics
    ///
    /// Panics when no order id has been issued yet.
    pub fn next_cancel(&mut self) -> CancelOrder {
        assert!(self.fresh_ids >= 1, "no issued ids to cancel");
        let sequence = self.next_sequence();
        CancelOrder {
            order_id: OrderId(1 + self.rng.below(self.fresh_ids)),
            account_id: self.next_account(),
            instrument_id: self.instrument,
            sequence: SequenceNumber(sequence),
        }
    }

    /// Distinct fresh order ids issued so far.
    #[must_use]
    pub const fn issued_order_ids(&self) -> u64 {
        self.fresh_ids
    }

    fn next_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("sequence space exhausted");
        sequence
    }

    fn next_account(&mut self) -> AccountId {
        AccountId(
            u32::try_from(1 + self.rng.below(u64::from(self.config.accounts)))
                .expect("small account"),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelOrder {
    pub id: u64,
    pub account: u32,
    pub quantity: u64,
    pub sequence: u64,
}

pub type ModelLevels = Vec<(i64, VecDeque<ModelOrder>)>;

/// One fill of a reference match: maker identity, trade price, traded amount.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelFill {
    pub maker_order_id: OrderId,
    pub price: PriceTicks,
    pub quantity: Quantity,
}

/// Reference array model: per-level dense FIFO with shift removal. Mirrors
/// the matching core's validation and rejection semantics exactly.
#[derive(Clone, Default, Debug)]
pub struct ModelBook {
    pub bids: ModelLevels,
    pub asks: ModelLevels,
}

impl ModelBook {
    /// Validation and preflight; mirrors atomic book rejection.
    ///
    /// # Errors
    ///
    /// Returns the mirrored rejection without mutating state.
    fn plan(
        &self,
        order: &NewOrder,
        report_capacity: usize,
        level_cap: usize,
        order_cap: usize,
    ) -> Result<Vec<i64>, RejectReason> {
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
        let mut crossing: Vec<i64> = maker_levels
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
        if order.time_in_force == TimeInForce::PostOnly && !crossing.is_empty() {
            return Err(RejectReason::PostOnlyWouldTrade);
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
        match order.time_in_force {
            TimeInForce::Fok => {
                if remaining > 0 {
                    return Err(RejectReason::InsufficientLiquidity);
                }
            }
            TimeInForce::Ioc => {
                // The discard itself is applied by `submit`.
            }
            TimeInForce::Gtc | TimeInForce::PostOnly => {
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
            }
        }
        Ok(crossing)
    }

    /// Applies a planned submission, returning the summary and fills in trade
    /// order (best price first, FIFO within a level).
    ///
    /// # Errors
    ///
    /// Returns the mirrored rejection without mutating state.
    ///
    /// # Panics
    ///
    /// Panics if the preflighted crossing level disappears between planning
    /// and application, which is a model bug.
    pub fn submit(
        &mut self,
        order: &NewOrder,
        report_capacity: usize,
        level_cap: usize,
        order_cap: usize,
    ) -> Result<(OrderState, Quantity, Quantity, Quantity, Vec<ModelFill>), RejectReason> {
        let crossing = self.plan(order, report_capacity, level_cap, order_cap)?;
        let mut remaining = order.quantity.0;
        let mut discarded = 0_u64;
        let maker_levels = match order.side {
            Side::Buy => &mut self.asks,
            Side::Sell => &mut self.bids,
        };
        let mut fills = Vec::new();
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
                fills.push(ModelFill {
                    maker_order_id: OrderId(front.id),
                    price: PriceTicks(*price),
                    quantity: Quantity(traded),
                });
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
        if remaining > 0
            && matches!(
                order.time_in_force,
                TimeInForce::Gtc | TimeInForce::PostOnly
            )
        {
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
                None => own_levels.push((order.price.0, VecDeque::from([resting]))),
            }
        } else if order.time_in_force == TimeInForce::Ioc {
            discarded = remaining;
            remaining = 0;
        }
        let filled = order.quantity.0 - remaining - discarded;
        let state = if filled == order.quantity.0 {
            OrderState::Filled
        } else if filled == 0 {
            OrderState::Accepted
        } else {
            OrderState::PartiallyFilled
        };
        Ok((
            state,
            Quantity(filled),
            Quantity(remaining),
            Quantity(discarded),
            fills,
        ))
    }

    /// Cancels by id, enforcing ownership.
    ///
    /// # Errors
    ///
    /// Returns unknown-order or ownership rejection without mutating state.
    /// Mirrors the book's owned amend: same-price reductions keep priority,
    /// everything else removes and re-adds at the destination tail. Repricing
    /// that would cross the opposing best rejects without mutation.
    ///
    /// # Errors
    ///
    /// Returns the mirrored rejection without mutating state.
    ///
    /// # Panics
    ///
    /// Panics if a located row disappears mid-replace, which is a model bug.
    #[allow(clippy::too_many_lines)]
    pub fn replace(
        &mut self,
        replace: &ReplaceOrder,
        level_cap: usize,
        order_cap: usize,
    ) -> Result<(u64, bool), RejectReason> {
        if replace.price.0 <= 0 {
            return Err(RejectReason::InvalidPrice);
        }
        if replace.quantity.0 == 0 {
            return Err(RejectReason::InvalidQuantity);
        }
        // Phase 1: immutable reads.
        let in_bids = Self::find_in(&self.bids, replace.order_id.0);
        let in_asks = Self::find_in(&self.asks, replace.order_id.0);
        let (side, position) = match (in_bids, in_asks) {
            (Some(position), _) => (Side::Buy, position),
            (_, Some(position)) => (Side::Sell, position),
            (None, None) => return Err(RejectReason::UnknownOrder),
        };
        let levels = match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        };
        let (level_price, queue_position, old_quantity, old_account) = {
            let (price, queue) = &levels[position];
            let queue_position = queue
                .iter()
                .position(|order| order.id == replace.order_id.0)
                .expect("located row");
            (
                *price,
                queue_position,
                queue[queue_position].quantity,
                queue[queue_position].account,
            )
        };
        if old_account != replace.account_id.0 {
            return Err(RejectReason::NotOrderOwner);
        }
        let priority_kept = replace.price.0 == level_price && replace.quantity.0 < old_quantity;
        if !priority_kept && replace.price.0 != level_price {
            let opposing = if side == Side::Buy {
                &self.asks
            } else {
                &self.bids
            };
            if let Some(&(best, _)) = opposing.iter().min_by(|x, y| {
                if side == Side::Buy {
                    x.0.cmp(&y.0)
                } else {
                    y.0.cmp(&x.0)
                }
            }) {
                let crosses = if side == Side::Buy {
                    replace.price.0 >= best
                } else {
                    replace.price.0 <= best
                };
                if crosses {
                    return Err(RejectReason::ReplaceWouldCross);
                }
            }
        }
        if !priority_kept {
            let dest_exists = levels.iter().any(|(price, _)| *price == replace.price.0);
            if dest_exists {
                let dest_index = levels
                    .iter()
                    .position(|(price, _)| *price == replace.price.0)
                    .expect("checked dest");
                let same_level = dest_index == position && replace.price.0 == level_price;
                if levels[dest_index].1.len() == order_cap && !same_level {
                    return Err(RejectReason::PriceLevelOrderCapacity);
                }
            } else {
                let source_empties = levels[position].1.len() == 1;
                if levels.len() == level_cap && !source_empties {
                    return Err(RejectReason::PriceLevelCapacity);
                }
            }
        }

        // Phase 2: mutation.
        let levels = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        if priority_kept {
            // In-place reduction: FIFO position and level stay untouched.
            let (_, queue) = &mut levels[position];
            let row = queue.get_mut(queue_position).expect("located row");
            row.quantity = replace.quantity.0;
        } else {
            let (_, queue) = &mut levels[position];
            queue.remove(queue_position);
            let source_emptied = queue.is_empty();
            if source_emptied {
                levels.remove(position);
            }
            let resting = ModelOrder {
                id: replace.order_id.0,
                account: replace.account_id.0,
                quantity: replace.quantity.0,
                sequence: replace.sequence.0,
            };
            match levels
                .iter_mut()
                .find(|(price, _)| *price == replace.price.0)
            {
                Some((_, queue)) => queue.push_back(resting),
                None => levels.push((replace.price.0, VecDeque::from([resting]))),
            }
        }
        Ok((old_quantity, !priority_kept))
    }
    fn find_in(levels: &ModelLevels, id: u64) -> Option<usize> {
        levels
            .iter()
            .position(|(_, queue)| queue.iter().any(|order| order.id == id))
    }

    /// Cancels by id, enforcing ownership.
    ///
    /// # Errors
    ///
    /// Returns unknown-order or ownership rejection without mutating state.
    pub fn cancel(&mut self, cancel: &CancelOrder) -> Result<(u64, u32, u64), RejectReason> {
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
            let Some(index) = queue.iter().position(|order| order.id == cancel.order_id.0) else {
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

/// Resting-state dump: `(side, price, id, quantity, sequence)` tuples, bids
/// then asks, prices ascending within a side, FIFO within a level.
pub type ModelDump = Vec<(Side, i64, u64, u64, u64)>;

#[must_use]
pub fn dump_model(model: &ModelBook) -> ModelDump {
    let mut dump = dump_levels(Side::Buy, &model.bids);
    dump.extend(dump_levels(Side::Sell, &model.asks));
    dump
}

fn dump_levels(side: Side, levels: &ModelLevels) -> ModelDump {
    let mut prices: Vec<i64> = levels.iter().map(|(price, _)| *price).collect();
    prices.sort_unstable();
    let mut dump = Vec::new();
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

/// Mirror of the engine's per-account limits for generated sessions.
#[derive(Clone, Copy, Debug)]
pub struct ModelLimits {
    pub max_quantity: u64,
    pub max_notional: u128,
    pub max_abs_position: i128,
    pub max_open_orders: u32,
    pub minimum_price: i64,
    pub maximum_price: i64,
}

/// Which layer rejected a mirrored command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelRejection {
    Risk(RejectReason),
    Book(RejectReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelNewOutcome {
    pub filled: Quantity,
    pub rested: Quantity,
    pub discarded: Quantity,
    pub fills: Vec<ModelFill>,
}

/// Outcome of an owned replace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelReplaced {
    pub order_id: OrderId,
    pub account_id: AccountId,
    pub old_quantity: Quantity,
    pub new_quantity: Quantity,
    pub price: PriceTicks,
    pub priority_lost: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelCancelled {
    pub order_id: OrderId,
    pub account_id: AccountId,
    pub quantity: Quantity,
}

#[derive(Debug)]
struct ModelAccount {
    limits: ModelLimits,
    settled_position: i128,
    reserved_buys: u128,
    reserved_sells: u128,
    open_orders: u32,
    killed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModelReservation {
    account_id: AccountId,
    side: Side,
    remaining: u64,
}

/// Gateway-level reference model mirroring sequencing, duplicate-id policy,
/// risk reservations, and matching outcomes for well-formed commands.
#[derive(Debug)]
pub struct ModelEngine {
    instrument: InstrumentId,
    level_cap: usize,
    order_cap: usize,
    book: ModelBook,
    accounts: BTreeMap<u32, ModelAccount>,
    reservations: BTreeMap<u64, ModelReservation>,
    maximum_received_order_id: Option<u64>,
    next_sequence: u64,
}

impl ModelEngine {
    #[must_use]
    pub fn new(instrument: InstrumentId, level_cap: usize, order_cap: usize) -> Self {
        Self {
            instrument,
            level_cap,
            order_cap,
            book: ModelBook::default(),
            accounts: BTreeMap::new(),
            reservations: BTreeMap::new(),
            maximum_received_order_id: None,
            next_sequence: 1,
        }
    }

    /// Registers an account with its limits.
    ///
    /// # Panics
    ///
    /// Panics on duplicate registration, which tests treat as setup bugs.
    pub fn register_account(&mut self, id: AccountId, limits: ModelLimits) {
        assert!(
            self.accounts
                .insert(
                    id.0,
                    ModelAccount {
                        limits,
                        settled_position: 0,
                        reserved_buys: 0,
                        reserved_sells: 0,
                        open_orders: 0,
                        killed: false,
                    }
                )
                .is_none(),
            "duplicate model account"
        );
    }

    /// Mirrors the gateway path for a new-order frame.
    ///
    /// # Errors
    ///
    /// Returns the mirrored risk or book rejection.
    ///
    /// # Panics
    ///
    /// Panics when the command sequence drifts from the session counter or an
    /// internal mirror invariant breaks; both indicate generator bugs.
    pub fn apply_new(
        &mut self,
        order: &NewOrder,
        report_capacity: usize,
    ) -> Result<ModelNewOutcome, ModelRejection> {
        assert_eq!(
            order.sequence.0, self.next_sequence,
            "sequence {} out of order",
            order.sequence.0
        );
        self.next_sequence = self.next_sequence.wrapping_add(1);
        // Gateway preflight: burned ids stay rejected, and the maximum moves
        // even when a later risk check rejects the command.
        if self
            .maximum_received_order_id
            .is_some_and(|maximum| order.order_id.0 <= maximum)
        {
            return Err(ModelRejection::Risk(RejectReason::DuplicateOrderId));
        }
        self.maximum_received_order_id = Some(order.order_id.0);

        let Some(account) = self.accounts.get(&order.account_id.0) else {
            return Err(ModelRejection::Risk(RejectReason::UnknownAccount));
        };
        if let Err(reason) = Self::evaluate_limits(account, order) {
            return Err(ModelRejection::Risk(reason));
        }
        {
            let account = self
                .accounts
                .get_mut(&order.account_id.0)
                .expect("checked account");
            match order.side {
                Side::Buy => {
                    account.reserved_buys = account
                        .reserved_buys
                        .wrapping_add(u128::from(order.quantity.0));
                }
                Side::Sell => {
                    account.reserved_sells = account
                        .reserved_sells
                        .wrapping_add(u128::from(order.quantity.0));
                }
            }
            account.open_orders = account.open_orders.wrapping_add(1);
        }
        self.reservations.insert(
            order.order_id.0,
            ModelReservation {
                account_id: order.account_id,
                side: order.side,
                remaining: order.quantity.0,
            },
        );

        if order.instrument_id != self.instrument {
            self.release(order.order_id.0);
            return Err(ModelRejection::Book(RejectReason::InvalidInstrument));
        }
        let planned = self
            .book
            .submit(order, report_capacity, self.level_cap, self.order_cap);
        match planned {
            Err(reason) => {
                // Mirrors settle(order_id, Quantity(0)) on book rejection.
                self.release(order.order_id.0);
                Err(ModelRejection::Book(reason))
            }
            Ok((_, filled, rested, discarded, fills)) => {
                for fill in &fills {
                    self.record_fill(fill.maker_order_id, fill.quantity.0);
                    self.record_fill(order.order_id, fill.quantity.0);
                }
                // Mirror the gateway discarding an IOC/FOK remainder.
                if rested.0 == 0 && filled.0 < order.quantity.0 {
                    self.release(order.order_id.0);
                }
                Ok(ModelNewOutcome {
                    filled,
                    rested,
                    discarded,
                    fills,
                })
            }
        }
    }

    /// Mirrors the gateway path for a cancel frame.
    ///
    /// # Errors
    ///
    /// Returns the mirrored risk or book rejection.
    ///
    /// # Panics
    ///
    /// Panics when the command sequence drifts from the session counter.
    pub fn apply_cancel(&mut self, cancel: &CancelOrder) -> Result<ModelCancelled, ModelRejection> {
        assert_eq!(
            cancel.sequence.0, self.next_sequence,
            "sequence {} out of order",
            cancel.sequence.0
        );
        self.next_sequence = self.next_sequence.wrapping_add(1);
        if cancel.instrument_id != self.instrument {
            return Err(ModelRejection::Book(RejectReason::InvalidInstrument));
        }
        let Some(reservation) = self.reservations.get(&cancel.order_id.0) else {
            return Err(ModelRejection::Risk(RejectReason::UnknownOrder));
        };
        if reservation.account_id != cancel.account_id {
            return Err(ModelRejection::Risk(RejectReason::NotOrderOwner));
        }
        let cancelled = self.book.cancel(cancel).map_err(ModelRejection::Book)?;
        self.release(cancel.order_id.0);
        Ok(ModelCancelled {
            order_id: cancel.order_id,
            account_id: cancel.account_id,
            quantity: Quantity(cancelled.2),
        })
    }

    /// Mirrors the gateway path for a replace frame.
    ///
    /// # Errors
    ///
    /// Returns the mirrored risk or book rejection.
    ///
    /// # Panics
    ///
    /// Panics when the command sequence drifts from the session counter or an
    /// internal invariant breaks.
    pub fn apply_replace(
        &mut self,
        replace: &ReplaceOrder,
    ) -> Result<ModelReplaced, ModelRejection> {
        assert_eq!(
            replace.sequence.0, self.next_sequence,
            "sequence {} out of order",
            replace.sequence.0
        );
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let Some(reservation) = self.reservations.get(&replace.order_id.0) else {
            return Err(ModelRejection::Risk(RejectReason::UnknownOrder));
        };
        if reservation.account_id != replace.account_id {
            return Err(ModelRejection::Risk(RejectReason::NotOrderOwner));
        }
        let (_, prior_remaining) = match self.adjust_reservation(replace) {
            Ok(amounts) => amounts,
            Err(reason) => return Err(ModelRejection::Risk(reason)),
        };
        if replace.instrument_id != self.instrument {
            self.restore_reservation(replace.order_id.0, prior_remaining);
            return Err(ModelRejection::Book(RejectReason::InvalidInstrument));
        }
        match self.book.replace(replace, self.level_cap, self.order_cap) {
            Ok((old_quantity, priority_lost)) => Ok(ModelReplaced {
                order_id: replace.order_id,
                account_id: replace.account_id,
                old_quantity: Quantity(old_quantity),
                new_quantity: Quantity(replace.quantity.0),
                price: replace.price,
                priority_lost,
            }),
            Err(reason) => {
                self.restore_reservation(replace.order_id.0, prior_remaining);
                Err(ModelRejection::Book(reason))
            }
        }
    }
    /// Projected position and open-order count, mirroring `account_snapshot`.
    #[must_use]
    pub fn account_view(&self, id: AccountId) -> Option<(i128, u32)> {
        let account = self.accounts.get(&id.0)?;
        let projected = account
            .settled_position
            .checked_add(i128::try_from(account.reserved_buys).ok()?)
            .and_then(|value| value.checked_sub(i128::try_from(account.reserved_sells).ok()?))?;
        Some((projected, account.open_orders))
    }

    /// Reserved exposure for one account side.
    #[must_use]
    pub fn reserved_total(&self, id: AccountId, side: Side) -> u128 {
        let Some(account) = self.accounts.get(&id.0) else {
            return 0;
        };
        match side {
            Side::Buy => account.reserved_buys,
            Side::Sell => account.reserved_sells,
        }
    }

    /// Panics unless reserved totals equal live reservation sums and every
    /// resting order owns a matching live reservation. Encodes "reservation
    /// equals live exposure".
    ///
    /// # Panics
    ///
    /// Panics on the first inconsistent account, reservation, or resting row.
    pub fn assert_consistent(&self) {
        let mut reserved_buys: BTreeMap<u32, u128> = BTreeMap::new();
        let mut reserved_sells: BTreeMap<u32, u128> = BTreeMap::new();
        let mut open_counts: BTreeMap<u32, u32> = BTreeMap::new();
        for (id, reservation) in &self.reservations {
            let totals = match reservation.side {
                Side::Buy => &mut reserved_buys,
                Side::Sell => &mut reserved_sells,
            };
            *totals.entry(reservation.account_id.0).or_insert(0) +=
                u128::from(reservation.remaining);
            *open_counts.entry(reservation.account_id.0).or_insert(0) += 1;
            assert!(reservation.remaining > 0, "reservation {id} emptied");
            let account = self
                .accounts
                .get(&reservation.account_id.0)
                .expect("known account");
            assert!(!account.killed, "killed account {id} keeps reservations");
        }
        for (id, account) in &self.accounts {
            assert_eq!(
                account.reserved_buys,
                reserved_buys.get(id).copied().unwrap_or(0),
                "buy reservation total for {id}"
            );
            assert_eq!(
                account.reserved_sells,
                reserved_sells.get(id).copied().unwrap_or(0),
                "sell reservation total for {id}"
            );
            assert_eq!(
                account.open_orders,
                open_counts.get(id).copied().unwrap_or(0),
                "open-order count for {id}"
            );
        }
        for (side, levels) in [(Side::Buy, &self.book.bids), (Side::Sell, &self.book.asks)] {
            for (_, queue) in levels {
                for resting in queue {
                    let reservation = self.reservations.get(&resting.id).unwrap_or_else(|| {
                        panic!("resting order {} lacks a reservation", resting.id)
                    });
                    assert_eq!(
                        reservation.remaining, resting.quantity,
                        "resting {}",
                        resting.id
                    );
                    assert_eq!(
                        reservation.account_id.0, resting.account,
                        "owner {}",
                        resting.id
                    );
                    assert_eq!(reservation.side, side, "side {}", resting.id);
                }
            }
        }
    }

    /// Debug helper for property diagnostics.
    #[must_use]
    pub fn reservation_of(&self, id: OrderId) -> Option<u64> {
        self.reservations.get(&id.0).map(|r| r.remaining)
    }

    /// Mirrors `adjust_reservation` for the model: limit-checked increase or
    /// immediate decrease of one reservation's total. Returns the released
    /// amount (zero for increases).
    fn adjust_reservation(&mut self, replace: &ReplaceOrder) -> Result<(u64, u64), RejectReason> {
        let Some(reservation) = self.reservations.get_mut(&replace.order_id.0) else {
            return Err(RejectReason::UnknownOrder);
        };
        let remaining = reservation.remaining;
        let new_total = replace.quantity.0;
        let side = reservation.side;
        let account_id = reservation.account_id;
        let prior = remaining;
        if new_total == remaining {
            return Ok((0, prior));
        }
        let account = self
            .accounts
            .get_mut(&account_id.0)
            .expect("reserved account");
        if new_total > remaining {
            let added = new_total - remaining;
            match side {
                Side::Buy => {
                    account.reserved_buys = account
                        .reserved_buys
                        .checked_add(u128::from(added))
                        .ok_or(RejectReason::ArithmeticOverflow)?;
                }
                Side::Sell => {
                    account.reserved_sells = account
                        .reserved_sells
                        .checked_add(u128::from(added))
                        .ok_or(RejectReason::ArithmeticOverflow)?;
                }
            }
            let absolute_price = replace
                .price
                .0
                .checked_abs()
                .ok_or(RejectReason::ArithmeticOverflow)?;
            let notional = u128::from(
                u64::try_from(absolute_price).map_err(|_| RejectReason::ArithmeticOverflow)?,
            )
            .checked_mul(u128::from(new_total))
            .ok_or(RejectReason::ArithmeticOverflow)?;
            if notional > account.limits.max_notional {
                return Err(RejectReason::NotionalLimit);
            }
            let maximum_position = account.limits.max_abs_position;
            let worst_long = account
                .settled_position
                .checked_add(i128::try_from(account.reserved_buys).unwrap_or(i128::MAX))
                .ok_or(RejectReason::ArithmeticOverflow)?;
            let worst_short = account
                .settled_position
                .checked_sub(i128::try_from(account.reserved_sells).unwrap_or(i128::MAX))
                .ok_or(RejectReason::ArithmeticOverflow)?;
            if worst_long > maximum_position || worst_short < -maximum_position {
                return Err(RejectReason::PositionLimit);
            }
        } else {
            let released = remaining - new_total;
            match side {
                Side::Buy => account.reserved_buys -= u128::from(released),
                Side::Sell => account.reserved_sells -= u128::from(released),
            }
        }
        let Some(reservation) = self.reservations.get_mut(&replace.order_id.0) else {
            unreachable!("reservation present");
        };
        reservation.remaining = new_total;
        Ok((remaining.saturating_sub(new_total), prior))
    }

    /// Restores a reservation total after a rolled-back book mutation.
    fn restore_reservation(&mut self, order_id: u64, prior_remaining: u64) {
        // The rollback mirrors the gateway re-adjusting with the prior total.
        let current = self
            .reservations
            .get(&order_id)
            .map_or(prior_remaining, |reservation| reservation.remaining);
        let delta = i128::from(prior_remaining) - i128::from(current);
        if delta == 0 {
            return;
        }
        let (side, account_id) = {
            let reservation = self.reservations.get(&order_id).expect("live");
            (reservation.side, reservation.account_id)
        };
        let account = self.accounts.get_mut(&account_id.0).expect("account");
        match side {
            Side::Buy => {
                account.reserved_buys =
                    (i128::try_from(account.reserved_buys).expect("fits") + delta).unsigned_abs();
            }
            Side::Sell => {
                account.reserved_sells =
                    (i128::try_from(account.reserved_sells).expect("fits") + delta).unsigned_abs();
            }
        }
        self.reservations
            .get_mut(&order_id)
            .expect("live")
            .remaining = prior_remaining;
    }
    /// Releases a reservation completely without a position change; mirrors
    /// `settle(order_id, Quantity(0))` and full cancel releases.
    fn release(&mut self, order_id: u64) {
        let Some(reservation) = self.reservations.remove(&order_id) else {
            panic!("release of unknown reservation {order_id}");
        };
        let account = self
            .accounts
            .get_mut(&reservation.account_id.0)
            .expect("reserved account");
        match reservation.side {
            Side::Buy => {
                account.reserved_buys -= u128::from(reservation.remaining);
            }
            Side::Sell => {
                account.reserved_sells -= u128::from(reservation.remaining);
            }
        }
        account.open_orders = account
            .open_orders
            .checked_sub(1)
            .expect("open-order underflow");
    }

    /// Mirrors `RiskEngine::record_fill`: traded amount leaves the reserved
    /// total and becomes signed position; emptied reservations close orders.
    fn record_fill(&mut self, order_id: OrderId, traded: u64) {
        assert!(traded > 0, "zero fill");
        let Some(reservation) = self.reservations.get_mut(&order_id.0) else {
            panic!("fill for unknown reservation {}", order_id.0);
        };
        assert!(
            traded <= reservation.remaining,
            "fill exceeds reservation for {}",
            order_id.0
        );
        reservation.remaining -= traded;
        let (side, account_id, emptied) = (
            reservation.side,
            reservation.account_id,
            reservation.remaining == 0,
        );
        let account = self
            .accounts
            .get_mut(&account_id.0)
            .expect("reserved account");
        match side {
            Side::Buy => {
                account.settled_position = account
                    .settled_position
                    .checked_add(i128::from(traded))
                    .expect("position overflow");
                account.reserved_buys -= u128::from(traded);
            }
            Side::Sell => {
                account.settled_position = account
                    .settled_position
                    .checked_sub(i128::from(traded))
                    .expect("position underflow");
                account.reserved_sells -= u128::from(traded);
            }
        }
        if emptied {
            account.open_orders = account
                .open_orders
                .checked_sub(1)
                .expect("open-order underflow");
            self.reservations.remove(&order_id.0);
        }
    }

    /// Pure limit evaluation mirroring `evaluate_limits`, including error
    /// precedence and checked arithmetic widths.
    fn evaluate_limits(account: &ModelAccount, order: &NewOrder) -> Result<(), RejectReason> {
        if account.killed {
            return Err(RejectReason::KillSwitch);
        }
        if order.quantity.0 > account.limits.max_quantity {
            return Err(RejectReason::QuantityLimit);
        }
        if order.price.0 < account.limits.minimum_price
            || order.price.0 > account.limits.maximum_price
        {
            return Err(RejectReason::PriceCollar);
        }
        let absolute_price = order
            .price
            .0
            .checked_abs()
            .ok_or(RejectReason::ArithmeticOverflow)?;
        let notional = u128::from(
            u64::try_from(absolute_price).map_err(|_| RejectReason::ArithmeticOverflow)?,
        )
        .checked_mul(u128::from(order.quantity.0))
        .ok_or(RejectReason::ArithmeticOverflow)?;
        if notional > account.limits.max_notional {
            return Err(RejectReason::NotionalLimit);
        }
        let (reserved_buys, reserved_sells) = match order.side {
            Side::Buy => (
                account
                    .reserved_buys
                    .checked_add(u128::from(order.quantity.0))
                    .ok_or(RejectReason::ArithmeticOverflow)?,
                account.reserved_sells,
            ),
            Side::Sell => (
                account.reserved_buys,
                account
                    .reserved_sells
                    .checked_add(u128::from(order.quantity.0))
                    .ok_or(RejectReason::ArithmeticOverflow)?,
            ),
        };
        let maximum_position = account.limits.max_abs_position;
        let worst_long = account
            .settled_position
            .checked_add(
                i128::try_from(reserved_buys).map_err(|_| RejectReason::ArithmeticOverflow)?,
            )
            .ok_or(RejectReason::ArithmeticOverflow)?;
        let worst_short = account
            .settled_position
            .checked_sub(
                i128::try_from(reserved_sells).map_err(|_| RejectReason::ArithmeticOverflow)?,
            )
            .ok_or(RejectReason::ArithmeticOverflow)?;
        if worst_long > maximum_position || worst_short < -maximum_position {
            return Err(RejectReason::PositionLimit);
        }
        let open_orders = account
            .open_orders
            .checked_add(1)
            .ok_or(RejectReason::ArithmeticOverflow)?;
        if open_orders > account.limits.max_open_orders {
            return Err(RejectReason::OpenOrderLimit);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_reproducible_and_bounded() {
        let mut first = Rng::new(42);
        let mut second = Rng::new(42);
        for _ in 0..16 {
            assert_eq!(first.next_u64(), second.next_u64());
        }
        let mut third = Rng::new(43);
        assert!(first.next_u64() != third.next_u64(), "seeds diverge");
        for bound in [1_u64, 3, 97] {
            for _ in 0..10_000 {
                assert!(first.below(bound) < bound);
            }
        }
    }

    fn gen_config(cancel_pct: u64, dup_pct: u64) -> GenConfig {
        GenConfig {
            accounts: 2,
            minimum_price: 99,
            maximum_price: 101,
            max_quantity: 3,
            cancel_probability_pct: cancel_pct,
            duplicate_id_probability_pct: dup_pct,
            ioc_probability_pct: 15,
            fok_probability_pct: 10,
            post_only_probability_pct: 10,
            replace_probability_pct: 30,
        }
    }

    #[test]
    fn command_sequences_are_strict_and_fresh_ids_increase() {
        let mut generator = CommandGen::new(gen_config(0, 0), InstrumentId(1), 7);
        for expected in 1..=200_u64 {
            let Command::New(order) = generator.next_command() else {
                panic!("cancel probability zero emitted a cancel");
            };
            assert_eq!(order.sequence.0, expected);
            assert_eq!(order.order_id.0, expected);
            assert!((99..=101).contains(&order.price.0), "price outside bounds");
            assert!((1..=3).contains(&order.quantity.0));
        }
        assert_eq!(generator.issued_order_ids(), 200);
    }

    #[test]
    fn duplicate_ids_reuse_issued_range_and_cancels_target_issued() {
        let mut generator = CommandGen::new(gen_config(30, 100), InstrumentId(1), 9);
        let first = generator.next_new();
        assert_eq!(first.order_id.0, 1);
        let mut seen_duplicate = false;
        let mut seen_replace = false;
        let mut seen_cancel = false;
        for step in 1..=400_u64 {
            match generator.next_command() {
                Command::New(order) => {
                    assert_eq!(order.sequence.0, step + 1);
                    if order.order_id.0 <= 1 {
                        seen_duplicate = true;
                    }
                }
                Command::Cancel(cancel) => {
                    assert_eq!(cancel.sequence.0, step + 1);
                    assert!(cancel.order_id.0 >= 1);
                    seen_cancel = true;
                }
                Command::Replace(replace) => {
                    assert_eq!(replace.sequence.0, step + 1);
                    assert!(replace.order_id.0 >= 1);
                    seen_replace = true;
                }
            }
        }
        assert!(seen_duplicate, "duplication never fired");
        assert!(seen_replace, "replaces never fired");
        assert!(seen_cancel, "cancels never fired");
    }

    fn generous_limits() -> ModelLimits {
        ModelLimits {
            max_quantity: 10,
            max_notional: 1_000_000,
            max_abs_position: 100,
            max_open_orders: 8,
            minimum_price: 1,
            maximum_price: 200,
        }
    }

    fn new_order(
        sequence: u64,
        id: u64,
        account: u32,
        price: i64,
        quantity: u64,
        side: Side,
    ) -> NewOrder {
        NewOrder {
            time_in_force: hft_types::TimeInForce::Gtc,
            order_id: OrderId(id),
            account_id: AccountId(account),
            instrument_id: InstrumentId(1),
            price: PriceTicks(price),
            quantity: Quantity(quantity),
            sequence: SequenceNumber(sequence),
            side,
        }
    }

    #[test]
    fn model_rest_cross_cancel_flow_matches_expectations() {
        let mut model = ModelEngine::new(InstrumentId(1), 8, 8);
        model.register_account(AccountId(1), generous_limits());
        model.register_account(AccountId(2), generous_limits());

        let outcome = model
            .apply_new(&new_order(1, 1, 1, 100, 5, Side::Sell), 8)
            .expect("rest sell");
        assert_eq!(outcome.rested, Quantity(5));
        assert!(outcome.fills.is_empty());
        model.assert_consistent();

        let outcome = model
            .apply_new(&new_order(2, 2, 2, 101, 5, Side::Buy), 8)
            .expect("cross sell");
        assert_eq!(outcome.filled, Quantity(5));
        assert_eq!(outcome.rested, Quantity(0));
        assert_eq!(outcome.fills.len(), 1);
        assert_eq!(outcome.fills[0].maker_order_id, OrderId(1));
        assert_eq!(outcome.fills[0].quantity, Quantity(5));
        assert_eq!(outcome.fills[0].price, PriceTicks(100));
        model.assert_consistent();
        assert_eq!(model.account_view(AccountId(1)), Some((-5, 0)));
        assert_eq!(model.account_view(AccountId(2)), Some((5, 0)));
        assert_eq!(model.reserved_total(AccountId(1), Side::Sell), 0);

        assert_eq!(
            model.apply_cancel(&CancelOrder {
                order_id: OrderId(2),
                account_id: AccountId(2),
                instrument_id: InstrumentId(1),
                sequence: SequenceNumber(3),
            }),
            Err(ModelRejection::Risk(RejectReason::UnknownOrder)),
            "terminal orders cannot be canceled"
        );
        assert_eq!(model.next_sequence, 4);

        let outcome = model
            .apply_new(&new_order(4, 3, 1, 99, 2, Side::Buy), 8)
            .expect("rest bid");
        assert_eq!(outcome.rested, Quantity(2));
        model.assert_consistent();

        let cancelled = model
            .apply_cancel(&CancelOrder {
                order_id: OrderId(3),
                account_id: AccountId(1),
                instrument_id: InstrumentId(1),
                sequence: SequenceNumber(5),
            })
            .expect("owner cancel");
        assert_eq!(cancelled.quantity, Quantity(2));
        model.assert_consistent();
        assert_eq!(model.account_view(AccountId(1)), Some((-5, 0)));
    }

    #[test]
    fn model_limit_rejections_preserve_state_except_id_watermark() {
        let mut model = ModelEngine::new(InstrumentId(1), 8, 8);
        model.register_account(AccountId(1), generous_limits());
        model.register_account(
            AccountId(2),
            ModelLimits {
                max_quantity: 2,
                ..generous_limits()
            },
        );

        assert_eq!(
            model.apply_new(&new_order(1, 1, 2, 100, 5, Side::Sell), 8),
            Err(ModelRejection::Risk(RejectReason::QuantityLimit))
        );
        model.assert_consistent();
        assert_eq!(model.account_view(AccountId(2)), Some((0, 0)));

        // Reusing the rejected id fails; a fresh id succeeds.
        assert_eq!(
            model.apply_new(&new_order(2, 1, 1, 100, 5, Side::Sell), 8),
            Err(ModelRejection::Risk(RejectReason::DuplicateOrderId))
        );

        // Open-order limit: one resting slot only.
        let mut tight = ModelEngine::new(InstrumentId(1), 8, 8);
        tight.register_account(
            AccountId(1),
            ModelLimits {
                max_open_orders: 1,
                ..generous_limits()
            },
        );
        tight
            .apply_new(&new_order(1, 1, 1, 100, 1, Side::Sell), 8)
            .expect("first rest");
        assert_eq!(
            tight.apply_new(&new_order(2, 2, 1, 99, 1, Side::Buy), 8),
            Err(ModelRejection::Risk(RejectReason::OpenOrderLimit))
        );
        tight.assert_consistent();
        tight
            .apply_cancel(&CancelOrder {
                order_id: OrderId(1),
                account_id: AccountId(1),
                instrument_id: InstrumentId(1),
                sequence: SequenceNumber(3),
            })
            .expect("cancel frees the slot");
        tight
            .apply_new(&new_order(4, 3, 1, 99, 1, Side::Buy), 8)
            .expect("accepted after release");
        tight.assert_consistent();
    }

    #[test]
    fn model_stays_consistent_under_generated_churn() {
        let mut model = ModelEngine::new(InstrumentId(1), 4, 4);
        for account in 1..=2_u32 {
            model.register_account(AccountId(account), generous_limits());
        }
        let mut generator = CommandGen::new(
            GenConfig {
                accounts: 2,
                minimum_price: 98,
                maximum_price: 102,
                max_quantity: 4,
                cancel_probability_pct: 40,
                duplicate_id_probability_pct: 5,
                ioc_probability_pct: 15,
                fok_probability_pct: 10,
                post_only_probability_pct: 10,
                replace_probability_pct: 30,
            },
            InstrumentId(1),
            0xfeed,
        );
        for _ in 0..600 {
            match generator.next_command() {
                Command::New(order) => {
                    let _ = model.apply_new(&order, 8);
                }
                Command::Cancel(cancel) => {
                    let _ = model.apply_cancel(&cancel);
                }
                Command::Replace(replace) => {
                    let _ = model.apply_replace(&replace);
                }
            }
            model.assert_consistent();
        }
    }
}
