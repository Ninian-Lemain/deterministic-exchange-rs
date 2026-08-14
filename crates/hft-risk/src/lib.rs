#![forbid(unsafe_code)]

use hft_types::{AccountId, NewOrder, OrderId, PriceTicks, Quantity, RejectReason, Side};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RiskLimits {
    pub max_quantity: Quantity,
    pub max_notional: u128,
    pub max_abs_position: Quantity,
    pub max_open_orders: u32,
    pub minimum_price: PriceTicks,
    pub maximum_price: PriceTicks,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationError {
    DuplicateAccount,
    AccountCapacity,
    InvalidLimits,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AccountState {
    id: AccountId,
    limits: RiskLimits,
    settled_position: i128,
    reserved_buys: u128,
    reserved_sells: u128,
    open_orders: u32,
    killed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Reservation {
    order_id: OrderId,
    account_id: AccountId,
    side: Side,
    quantity: Quantity,
}

/// Deterministic, fixed-capacity pre-trade risk state.
///
/// Position is conservative: buy and sell reservations are tracked separately
/// so opposing open orders cannot cancel worst-case exposure. Order IDs must
/// increase monotonically within a session, preventing reuse without an
/// unbounded historical set. `settle` converts a reservation to its filled
/// quantity when an order is terminal.
#[derive(Debug)]
pub struct RiskEngine<const ACCOUNTS: usize, const ORDERS: usize> {
    accounts: [Option<AccountState>; ACCOUNTS],
    reservations: [Option<Reservation>; ORDERS],
    maximum_order_id: Option<OrderId>,
    killed: bool,
}

impl<const ACCOUNTS: usize, const ORDERS: usize> RiskEngine<ACCOUNTS, ORDERS> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            accounts: [None; ACCOUNTS],
            reservations: [None; ORDERS],
            maximum_order_id: None,
            killed: false,
        }
    }

    /// # Errors
    ///
    /// Returns a duplicate, invalid-limits, or account-capacity error.
    pub fn register_account(
        &mut self,
        id: AccountId,
        limits: RiskLimits,
    ) -> Result<(), RegistrationError> {
        if limits.max_quantity.0 == 0
            || limits.max_open_orders == 0
            || limits.minimum_price.0 > limits.maximum_price.0
        {
            return Err(RegistrationError::InvalidLimits);
        }
        if self
            .accounts
            .iter()
            .flatten()
            .any(|account| account.id == id)
        {
            return Err(RegistrationError::DuplicateAccount);
        }
        let Some(slot) = self.accounts.iter_mut().find(|slot| slot.is_none()) else {
            return Err(RegistrationError::AccountCapacity);
        };
        *slot = Some(AccountState {
            id,
            limits,
            settled_position: 0,
            reserved_buys: 0,
            reserved_sells: 0,
            open_orders: 0,
            killed: false,
        });
        Ok(())
    }

    pub const fn set_kill_switch(&mut self, killed: bool) {
        self.killed = killed;
    }

    /// # Errors
    ///
    /// Returns [`RejectReason::UnknownAccount`] for an unregistered account.
    pub fn set_account_kill_switch(
        &mut self,
        account_id: AccountId,
        killed: bool,
    ) -> Result<(), RejectReason> {
        let account = self
            .accounts
            .iter_mut()
            .flatten()
            .find(|account| account.id == account_id)
            .ok_or(RejectReason::UnknownAccount)?;
        account.killed = killed;
        Ok(())
    }

    /// # Errors
    ///
    /// Returns the first deterministic limit, arithmetic, duplicate, kill, or
    /// fixed-capacity rejection. Rejection does not mutate risk state.
    pub fn check_and_reserve(&mut self, order: NewOrder) -> Result<(), RejectReason> {
        if self.killed {
            return Err(RejectReason::KillSwitch);
        }
        if order.quantity.0 == 0 {
            return Err(RejectReason::InvalidQuantity);
        }
        if self
            .maximum_order_id
            .is_some_and(|maximum| order.order_id <= maximum)
        {
            return Err(RejectReason::DuplicateOrderId);
        }
        let reservation_slot = self
            .reservations
            .iter()
            .position(Option::is_none)
            .ok_or(RejectReason::OrderCapacity)?;
        let account_index = self
            .accounts
            .iter()
            .position(|entry| entry.is_some_and(|account| account.id == order.account_id))
            .ok_or(RejectReason::UnknownAccount)?;
        let account = self.accounts[account_index].ok_or(RejectReason::UnknownAccount)?;
        if account.killed {
            return Err(RejectReason::KillSwitch);
        }
        if order.quantity.0 > account.limits.max_quantity.0 {
            return Err(RejectReason::QuantityLimit);
        }
        if order.price.0 < account.limits.minimum_price.0
            || order.price.0 > account.limits.maximum_price.0
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
        let maximum_position = i128::from(account.limits.max_abs_position.0);
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

        self.accounts[account_index] = Some(AccountState {
            reserved_buys,
            reserved_sells,
            open_orders,
            ..account
        });
        self.reservations[reservation_slot] = Some(Reservation {
            order_id: order.order_id,
            account_id: order.account_id,
            side: order.side,
            quantity: order.quantity,
        });
        self.maximum_order_id = Some(order.order_id);
        Ok(())
    }

    /// Releases unfilled exposure and retains filled exposure as position.
    ///
    /// # Errors
    ///
    /// Returns an explicit state or arithmetic rejection for an unknown order
    /// or an invalid filled quantity. Rejection does not mutate state.
    pub fn settle(
        &mut self,
        order_id: OrderId,
        filled_quantity: Quantity,
    ) -> Result<(), RejectReason> {
        let reservation_index = self
            .reservations
            .iter()
            .position(|entry| entry.is_some_and(|item| item.order_id == order_id))
            .ok_or(RejectReason::UnknownOrder)?;
        let reservation =
            self.reservations[reservation_index].ok_or(RejectReason::ArithmeticOverflow)?;
        if filled_quantity.0 > reservation.quantity.0 {
            return Err(RejectReason::InvalidQuantity);
        }
        let account_index = self
            .accounts
            .iter()
            .position(|entry| entry.is_some_and(|item| item.id == reservation.account_id))
            .ok_or(RejectReason::UnknownAccount)?;
        let account = self.accounts[account_index].ok_or(RejectReason::UnknownAccount)?;
        let (settled_position, reserved_buys, reserved_sells) = match reservation.side {
            Side::Buy => (
                account
                    .settled_position
                    .checked_add(i128::from(filled_quantity.0))
                    .ok_or(RejectReason::ArithmeticOverflow)?,
                account
                    .reserved_buys
                    .checked_sub(u128::from(reservation.quantity.0))
                    .ok_or(RejectReason::ArithmeticOverflow)?,
                account.reserved_sells,
            ),
            Side::Sell => (
                account
                    .settled_position
                    .checked_sub(i128::from(filled_quantity.0))
                    .ok_or(RejectReason::ArithmeticOverflow)?,
                account.reserved_buys,
                account
                    .reserved_sells
                    .checked_sub(u128::from(reservation.quantity.0))
                    .ok_or(RejectReason::ArithmeticOverflow)?,
            ),
        };
        let open_orders = account
            .open_orders
            .checked_sub(1)
            .ok_or(RejectReason::ArithmeticOverflow)?;
        self.accounts[account_index] = Some(AccountState {
            settled_position,
            reserved_buys,
            reserved_sells,
            open_orders,
            ..account
        });
        self.reservations[reservation_index] = None;
        Ok(())
    }

    /// Applies a fill while retaining the signed projected exposure. When the
    /// reservation is fully filled, the order is closed but the resulting
    /// position remains in the projected position.
    ///
    /// # Errors
    ///
    /// Returns an explicit state or quantity rejection when the reservation
    /// does not exist or the fill exceeds the reserved remainder.
    pub fn record_fill(
        &mut self,
        order_id: OrderId,
        filled_quantity: Quantity,
    ) -> Result<(), RejectReason> {
        if filled_quantity.0 == 0 {
            return Err(RejectReason::InvalidQuantity);
        }
        let reservation_index = self
            .reservations
            .iter()
            .position(|entry| entry.is_some_and(|item| item.order_id == order_id))
            .ok_or(RejectReason::UnknownOrder)?;
        let mut reservation =
            self.reservations[reservation_index].ok_or(RejectReason::ArithmeticOverflow)?;
        reservation.quantity.0 = reservation
            .quantity
            .0
            .checked_sub(filled_quantity.0)
            .ok_or(RejectReason::InvalidQuantity)?;
        let account_index = self
            .accounts
            .iter()
            .position(|entry| entry.is_some_and(|item| item.id == reservation.account_id))
            .ok_or(RejectReason::UnknownAccount)?;
        let account = self.accounts[account_index].ok_or(RejectReason::UnknownAccount)?;
        let (settled_position, reserved_buys, reserved_sells) = match reservation.side {
            Side::Buy => (
                account
                    .settled_position
                    .checked_add(i128::from(filled_quantity.0))
                    .ok_or(RejectReason::ArithmeticOverflow)?,
                account
                    .reserved_buys
                    .checked_sub(u128::from(filled_quantity.0))
                    .ok_or(RejectReason::ArithmeticOverflow)?,
                account.reserved_sells,
            ),
            Side::Sell => (
                account
                    .settled_position
                    .checked_sub(i128::from(filled_quantity.0))
                    .ok_or(RejectReason::ArithmeticOverflow)?,
                account.reserved_buys,
                account
                    .reserved_sells
                    .checked_sub(u128::from(filled_quantity.0))
                    .ok_or(RejectReason::ArithmeticOverflow)?,
            ),
        };
        let open_orders = if reservation.quantity.0 == 0 {
            account
                .open_orders
                .checked_sub(1)
                .ok_or(RejectReason::ArithmeticOverflow)?
        } else {
            account.open_orders
        };
        self.accounts[account_index] = Some(AccountState {
            settled_position,
            reserved_buys,
            reserved_sells,
            open_orders,
            ..account
        });
        if reservation.quantity.0 == 0 {
            self.reservations[reservation_index] = None;
        } else {
            self.reservations[reservation_index] = Some(reservation);
        }
        Ok(())
    }

    /// Validates ownership of an open reservation without mutation.
    ///
    /// # Errors
    ///
    /// Returns unknown-order or ownership rejection.
    pub fn can_cancel(&self, order_id: OrderId, account_id: AccountId) -> Result<(), RejectReason> {
        let reservation = self
            .reservations
            .iter()
            .flatten()
            .find(|reservation| reservation.order_id == order_id)
            .ok_or(RejectReason::UnknownOrder)?;
        if reservation.account_id != account_id {
            return Err(RejectReason::NotOrderOwner);
        }
        Ok(())
    }

    /// Releases the full remaining reservation for an owned canceled order.
    ///
    /// # Errors
    ///
    /// Returns unknown-order, ownership, or internal arithmetic rejection.
    pub fn cancel_reservation(
        &mut self,
        order_id: OrderId,
        account_id: AccountId,
    ) -> Result<Quantity, RejectReason> {
        self.can_cancel(order_id, account_id)?;
        let remaining = self
            .reservations
            .iter()
            .flatten()
            .find(|reservation| reservation.order_id == order_id)
            .map(|reservation| reservation.quantity)
            .ok_or(RejectReason::UnknownOrder)?;
        self.settle(order_id, Quantity(0))?;
        Ok(remaining)
    }

    #[must_use]
    pub fn stable_digest(&self) -> u64 {
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        mix(&mut digest, u64::from(self.killed));
        mix(
            &mut digest,
            self.maximum_order_id.map_or(0, |order_id| order_id.0),
        );
        for account in self.accounts.iter().flatten() {
            mix(&mut digest, u64::from(account.id.0));
            let position_bytes = account.settled_position.to_be_bytes();
            mix(
                &mut digest,
                u64::from_ne_bytes([
                    position_bytes[0],
                    position_bytes[1],
                    position_bytes[2],
                    position_bytes[3],
                    position_bytes[4],
                    position_bytes[5],
                    position_bytes[6],
                    position_bytes[7],
                ]),
            );
            for exposure in [account.reserved_buys, account.reserved_sells] {
                let bytes = exposure.to_be_bytes();
                mix(
                    &mut digest,
                    u64::from_ne_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7],
                    ]),
                );
                mix(
                    &mut digest,
                    u64::from_ne_bytes([
                        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                        bytes[15],
                    ]),
                );
            }
            mix(
                &mut digest,
                u64::from_ne_bytes([
                    position_bytes[8],
                    position_bytes[9],
                    position_bytes[10],
                    position_bytes[11],
                    position_bytes[12],
                    position_bytes[13],
                    position_bytes[14],
                    position_bytes[15],
                ]),
            );
            mix(&mut digest, u64::from(account.open_orders));
            mix(&mut digest, u64::from(account.killed));
        }
        for reservation in self.reservations.iter().flatten() {
            mix(&mut digest, reservation.order_id.0);
            mix(&mut digest, reservation.quantity.0);
        }
        digest
    }

    #[must_use]
    pub fn account_snapshot(&self, id: AccountId) -> Option<(i128, u32)> {
        self.accounts
            .iter()
            .flatten()
            .find(|account| account.id == id)
            .and_then(|account| {
                let buys = i128::try_from(account.reserved_buys).ok()?;
                let sells = i128::try_from(account.reserved_sells).ok()?;
                let projected = account
                    .settled_position
                    .checked_add(buys)?
                    .checked_sub(sells)?;
                Some((projected, account.open_orders))
            })
    }
}

impl<const ACCOUNTS: usize, const ORDERS: usize> Default for RiskEngine<ACCOUNTS, ORDERS> {
    fn default() -> Self {
        Self::new()
    }
}

fn mix(digest: &mut u64, value: u64) {
    *digest ^= value;
    *digest = digest.wrapping_mul(0x0000_0100_0000_01b3);
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_types::{InstrumentId, SequenceNumber};

    fn limits() -> RiskLimits {
        RiskLimits {
            max_quantity: Quantity(10),
            max_notional: 1_000,
            max_abs_position: Quantity(12),
            max_open_orders: 2,
            minimum_price: PriceTicks(1),
            maximum_price: PriceTicks(100),
        }
    }

    fn order(id: u64, side: Side, quantity: u64) -> NewOrder {
        NewOrder {
            order_id: OrderId(id),
            account_id: AccountId(7),
            instrument_id: InstrumentId(1),
            price: PriceTicks(10),
            quantity: Quantity(quantity),
            sequence: SequenceNumber(id),
            side,
        }
    }

    #[test]
    fn checks_limits_without_mutating_on_reject() {
        let mut risk = RiskEngine::<1, 2>::new();
        risk.register_account(AccountId(7), limits())
            .expect("valid limits");
        assert_eq!(
            risk.check_and_reserve(order(1, Side::Buy, 11)),
            Err(RejectReason::QuantityLimit)
        );
        assert_eq!(risk.account_snapshot(AccountId(7)), Some((0, 0)));
        assert_eq!(risk.check_and_reserve(order(1, Side::Buy, 8)), Ok(()));
        assert_eq!(
            risk.check_and_reserve(order(1, Side::Sell, 1)),
            Err(RejectReason::DuplicateOrderId)
        );
        assert_eq!(risk.account_snapshot(AccountId(7)), Some((8, 1)));
    }

    #[test]
    fn enforces_projected_position_and_settles_unfilled() {
        let mut risk = RiskEngine::<1, 2>::new();
        risk.register_account(AccountId(7), limits())
            .expect("valid limits");
        assert_eq!(risk.check_and_reserve(order(1, Side::Buy, 8)), Ok(()));
        assert_eq!(
            risk.check_and_reserve(order(2, Side::Buy, 5)),
            Err(RejectReason::PositionLimit)
        );
        assert_eq!(risk.settle(OrderId(1), Quantity(3)), Ok(()));
        assert_eq!(risk.account_snapshot(AccountId(7)), Some((3, 0)));
    }

    #[test]
    fn deterministic_sequence_has_stable_digest() {
        let mut first = RiskEngine::<1, 4>::new();
        let mut second = RiskEngine::<1, 4>::new();
        for engine in [&mut first, &mut second] {
            engine
                .register_account(AccountId(7), limits())
                .expect("valid limits");
            engine
                .check_and_reserve(order(1, Side::Buy, 2))
                .expect("within limits");
            engine
                .check_and_reserve(order(2, Side::Sell, 1))
                .expect("within limits");
        }
        assert_eq!(first.stable_digest(), second.stable_digest());
    }

    #[test]
    fn fills_close_order_without_releasing_position() {
        let mut risk = RiskEngine::<1, 2>::new();
        risk.register_account(AccountId(7), limits())
            .expect("valid limits");
        risk.check_and_reserve(order(1, Side::Buy, 5))
            .expect("within limits");
        risk.record_fill(OrderId(1), Quantity(2))
            .expect("partial fill");
        assert_eq!(risk.account_snapshot(AccountId(7)), Some((5, 1)));
        risk.record_fill(OrderId(1), Quantity(3))
            .expect("terminal fill");
        assert_eq!(risk.account_snapshot(AccountId(7)), Some((5, 0)));
    }

    #[test]
    fn completed_and_out_of_order_ids_are_not_reusable() {
        let mut risk = RiskEngine::<1, 2>::new();
        risk.register_account(AccountId(7), limits())
            .expect("valid limits");
        risk.check_and_reserve(order(10, Side::Buy, 1))
            .expect("first order");
        risk.record_fill(OrderId(10), Quantity(1))
            .expect("terminal fill");
        assert_eq!(
            risk.check_and_reserve(order(10, Side::Buy, 1)),
            Err(RejectReason::DuplicateOrderId)
        );
        assert_eq!(
            risk.check_and_reserve(order(9, Side::Buy, 1)),
            Err(RejectReason::DuplicateOrderId)
        );
    }

    #[test]
    fn opposing_reservations_do_not_cancel_worst_case_exposure() {
        let mut risk = RiskEngine::<1, 4>::new();
        risk.register_account(AccountId(7), limits())
            .expect("valid limits");
        risk.check_and_reserve(order(1, Side::Buy, 10))
            .expect("buy within limit");
        risk.check_and_reserve(order(2, Side::Sell, 10))
            .expect("sell within limit");
        assert_eq!(
            risk.check_and_reserve(order(3, Side::Buy, 3)),
            Err(RejectReason::PositionLimit)
        );
    }

    #[test]
    fn cancel_releases_only_owned_remaining_reservation() {
        let mut risk = RiskEngine::<2, 4>::new();
        risk.register_account(AccountId(7), limits())
            .expect("first account");
        risk.register_account(AccountId(8), limits())
            .expect("second account");
        risk.check_and_reserve(order(1, Side::Buy, 5))
            .expect("reserve order");
        risk.record_fill(OrderId(1), Quantity(2))
            .expect("partial fill");
        assert_eq!(
            risk.cancel_reservation(OrderId(1), AccountId(8)),
            Err(RejectReason::NotOrderOwner)
        );
        assert_eq!(
            risk.cancel_reservation(OrderId(1), AccountId(7)),
            Ok(Quantity(3))
        );
        assert_eq!(risk.account_snapshot(AccountId(7)), Some((2, 0)));
    }
}
