#![forbid(unsafe_code)]

use hft_io::RxFrame;
use hft_types::{
    AccountId, CancelOrder, InstrumentId, NewOrder, OrderId, PriceTicks, Quantity, ReplaceOrder,
    SequenceNumber, Side, TimeInForce,
};

pub const PROTOCOL_VERSION: u8 = 2;
pub const NEW_ORDER_TYPE: u8 = 1;
pub const CANCEL_ORDER_TYPE: u8 = 2;
pub const REPLACE_ORDER_TYPE: u8 = 3;
pub const NEW_ORDER_LEN: usize = 46;
const NEW_ORDER_WIRE_LEN: u16 = 46;
pub const CANCEL_ORDER_LEN: usize = 28;
const CANCEL_ORDER_WIRE_LEN: u16 = 28;
pub const REPLACE_ORDER_LEN: usize = 44;
const REPLACE_ORDER_WIRE_LEN: u16 = 44;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseError {
    TruncatedHeader,
    UnsupportedVersion,
    UnknownMessageType,
    InvalidLength,
    InvalidSide,
    InvalidTimeInForce,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BorrowedMessage<'frame> {
    NewOrder(BorrowedNewOrder<'frame>),
    CancelOrder(BorrowedCancelOrder<'frame>),
    ReplaceOrder(BorrowedReplaceOrder<'frame>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedNewOrder<'frame> {
    bytes: &'frame [u8],
}

impl BorrowedNewOrder<'_> {
    #[must_use]
    pub fn to_owned(self) -> NewOrder {
        NewOrder {
            time_in_force: match self.bytes[45] {
                value if value == TimeInForce::Gtc as u8 => TimeInForce::Gtc,
                value if value == TimeInForce::Ioc as u8 => TimeInForce::Ioc,
                value if value == TimeInForce::Fok as u8 => TimeInForce::Fok,
                _ => TimeInForce::PostOnly,
            },
            order_id: OrderId(read_u64(self.bytes, 4)),
            account_id: AccountId(read_u32(self.bytes, 12)),
            instrument_id: InstrumentId(read_u32(self.bytes, 16)),
            price: PriceTicks(read_i64(self.bytes, 20)),
            quantity: Quantity(read_u64(self.bytes, 28)),
            sequence: SequenceNumber(read_u64(self.bytes, 36)),
            side: if self.bytes[44] == Side::Buy as u8 {
                Side::Buy
            } else {
                Side::Sell
            },
        }
    }

    #[must_use]
    pub const fn wire_bytes(&self) -> &[u8] {
        self.bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedCancelOrder<'frame> {
    bytes: &'frame [u8],
}

impl BorrowedCancelOrder<'_> {
    #[must_use]
    pub fn to_owned(self) -> CancelOrder {
        CancelOrder {
            order_id: OrderId(read_u64(self.bytes, 4)),
            account_id: AccountId(read_u32(self.bytes, 12)),
            instrument_id: InstrumentId(read_u32(self.bytes, 16)),
            sequence: SequenceNumber(read_u64(self.bytes, 20)),
        }
    }

    #[must_use]
    pub const fn wire_bytes(&self) -> &[u8] {
        self.bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedReplaceOrder<'frame> {
    bytes: &'frame [u8],
}

impl BorrowedReplaceOrder<'_> {
    #[must_use]
    pub fn to_owned(self) -> ReplaceOrder {
        ReplaceOrder {
            order_id: OrderId(read_u64(self.bytes, 4)),
            account_id: AccountId(read_u32(self.bytes, 12)),
            instrument_id: InstrumentId(read_u32(self.bytes, 16)),
            sequence: SequenceNumber(read_u64(self.bytes, 20)),
            price: PriceTicks(read_i64(self.bytes, 28)),
            quantity: Quantity(read_u64(self.bytes, 36)),
        }
    }

    #[must_use]
    pub const fn wire_bytes(&self) -> &[u8] {
        self.bytes
    }
}

impl BorrowedMessage<'_> {
    /// Reads the session sequence from any message kind without owning it.
    ///
    /// # Panics
    ///
    /// Panics only if `parse_message` accepted the frame (offsets are
    /// proven in-bounds by the length check).
    #[must_use]
    pub fn sequence(&self) -> SequenceNumber {
        match self {
            BorrowedMessage::NewOrder(msg) => {
                SequenceNumber(u64::from_be_bytes(msg.bytes[36..44].try_into().unwrap()))
            }
            BorrowedMessage::CancelOrder(msg) => {
                SequenceNumber(u64::from_be_bytes(msg.bytes[20..28].try_into().unwrap()))
            }
            BorrowedMessage::ReplaceOrder(msg) => {
                SequenceNumber(u64::from_be_bytes(msg.bytes[20..28].try_into().unwrap()))
            }
        }
    }
}

/// # Errors
///
/// Returns a specific [`ParseError`] for an invalid header, length, type, or
/// side before typed interpretation is allowed.
pub fn parse_message<'frame>(
    frame: &'frame RxFrame<'_>,
) -> Result<BorrowedMessage<'frame>, ParseError> {
    let bytes = frame.bytes();
    if bytes.len() < 4 {
        return Err(ParseError::TruncatedHeader);
    }
    if bytes[0] != PROTOCOL_VERSION {
        return Err(ParseError::UnsupportedVersion);
    }
    let declared = usize::from(u16::from_be_bytes([bytes[2], bytes[3]]));
    if bytes.len() != declared {
        return Err(ParseError::InvalidLength);
    }
    match bytes[1] {
        NEW_ORDER_TYPE => {
            if declared != NEW_ORDER_LEN {
                return Err(ParseError::InvalidLength);
            }
            if !matches!(bytes[44], value if value == Side::Buy as u8 || value == Side::Sell as u8)
            {
                return Err(ParseError::InvalidSide);
            }
            if !matches!(
                bytes[45],
                value if value == TimeInForce::Gtc as u8
                    || value == TimeInForce::Ioc as u8
                    || value == TimeInForce::Fok as u8
                    || value == TimeInForce::PostOnly as u8
            ) {
                return Err(ParseError::InvalidTimeInForce);
            }
            Ok(BorrowedMessage::NewOrder(BorrowedNewOrder { bytes }))
        }
        CANCEL_ORDER_TYPE => {
            if declared != CANCEL_ORDER_LEN {
                return Err(ParseError::InvalidLength);
            }
            Ok(BorrowedMessage::CancelOrder(BorrowedCancelOrder { bytes }))
        }
        REPLACE_ORDER_TYPE => {
            if declared != REPLACE_ORDER_LEN {
                return Err(ParseError::InvalidLength);
            }
            Ok(BorrowedMessage::ReplaceOrder(BorrowedReplaceOrder {
                bytes,
            }))
        }
        _ => Err(ParseError::UnknownMessageType),
    }
}

#[must_use]
pub fn encode_new_order(order: NewOrder) -> [u8; NEW_ORDER_LEN] {
    let mut bytes = [0_u8; NEW_ORDER_LEN];
    bytes[0] = PROTOCOL_VERSION;
    bytes[1] = NEW_ORDER_TYPE;
    bytes[2..4].copy_from_slice(&NEW_ORDER_WIRE_LEN.to_be_bytes());
    bytes[4..12].copy_from_slice(&order.order_id.0.to_be_bytes());
    bytes[12..16].copy_from_slice(&order.account_id.0.to_be_bytes());
    bytes[16..20].copy_from_slice(&order.instrument_id.0.to_be_bytes());
    bytes[20..28].copy_from_slice(&order.price.0.to_be_bytes());
    bytes[28..36].copy_from_slice(&order.quantity.0.to_be_bytes());
    bytes[36..44].copy_from_slice(&order.sequence.0.to_be_bytes());
    bytes[44] = order.side as u8;
    bytes[45] = order.time_in_force as u8;
    bytes
}

#[must_use]
pub fn encode_cancel_order(cancel: CancelOrder) -> [u8; CANCEL_ORDER_LEN] {
    let mut bytes = [0_u8; CANCEL_ORDER_LEN];
    bytes[0] = PROTOCOL_VERSION;
    bytes[1] = CANCEL_ORDER_TYPE;
    bytes[2..4].copy_from_slice(&CANCEL_ORDER_WIRE_LEN.to_be_bytes());
    bytes[4..12].copy_from_slice(&cancel.order_id.0.to_be_bytes());
    bytes[12..16].copy_from_slice(&cancel.account_id.0.to_be_bytes());
    bytes[16..20].copy_from_slice(&cancel.instrument_id.0.to_be_bytes());
    bytes[20..28].copy_from_slice(&cancel.sequence.0.to_be_bytes());
    bytes
}

#[must_use]
pub fn encode_replace_order(replace: ReplaceOrder) -> [u8; REPLACE_ORDER_LEN] {
    let mut bytes = [0_u8; REPLACE_ORDER_LEN];
    bytes[0] = PROTOCOL_VERSION;
    bytes[1] = REPLACE_ORDER_TYPE;
    bytes[2..4].copy_from_slice(&REPLACE_ORDER_WIRE_LEN.to_be_bytes());
    bytes[4..12].copy_from_slice(&replace.order_id.0.to_be_bytes());
    bytes[12..16].copy_from_slice(&replace.account_id.0.to_be_bytes());
    bytes[16..20].copy_from_slice(&replace.instrument_id.0.to_be_bytes());
    bytes[20..28].copy_from_slice(&replace.sequence.0.to_be_bytes());
    bytes[28..36].copy_from_slice(&replace.price.0.to_be_bytes());
    bytes[36..44].copy_from_slice(&replace.quantity.0.to_be_bytes());
    bytes
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn read_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> NewOrder {
        NewOrder {
            time_in_force: hft_types::TimeInForce::Gtc,
            order_id: OrderId(10),
            account_id: AccountId(20),
            instrument_id: InstrumentId(30),
            price: PriceTicks(40),
            quantity: Quantity(50),
            sequence: SequenceNumber(60),
            side: Side::Sell,
        }
    }

    #[test]
    fn round_trip_new_order() {
        let bytes = encode_new_order(order());
        let frame = RxFrame::from_bytes(&bytes);
        let BorrowedMessage::NewOrder(parsed) = parse_message(&frame).expect("valid frame") else {
            panic!("expected new order");
        };
        assert_eq!(parsed.to_owned(), order());
        assert_eq!(parsed.wire_bytes().as_ptr(), bytes.as_ptr());
    }

    #[test]
    fn round_trip_cancel_order() {
        let cancel = CancelOrder {
            order_id: OrderId(10),
            account_id: AccountId(20),
            instrument_id: InstrumentId(30),
            sequence: SequenceNumber(40),
        };
        let bytes = encode_cancel_order(cancel);
        let frame = RxFrame::from_bytes(&bytes);
        let BorrowedMessage::CancelOrder(parsed) = parse_message(&frame).expect("valid cancel")
        else {
            panic!("expected cancel order");
        };
        assert_eq!(parsed.to_owned(), cancel);
        assert_eq!(parsed.wire_bytes().as_ptr(), bytes.as_ptr());
    }

    #[test]
    fn every_truncation_is_rejected() {
        let bytes = encode_new_order(order());
        for length in 0..NEW_ORDER_LEN {
            let frame = RxFrame::from_bytes(&bytes[..length]);
            assert!(parse_message(&frame).is_err(), "accepted length {length}");
        }
        let cancel = encode_cancel_order(CancelOrder {
            order_id: OrderId(1),
            account_id: AccountId(2),
            instrument_id: InstrumentId(3),
            sequence: SequenceNumber(4),
        });
        for length in 0..CANCEL_ORDER_LEN {
            let frame = RxFrame::from_bytes(&cancel[..length]);
            assert!(
                parse_message(&frame).is_err(),
                "accepted cancel length {length}"
            );
        }
    }

    #[test]
    fn rejects_each_invalid_discriminant() {
        let mut bytes = encode_new_order(order());
        bytes[0] = 9;
        assert_eq!(
            parse_message(&RxFrame::from_bytes(&bytes)),
            Err(ParseError::UnsupportedVersion)
        );
        bytes = encode_new_order(order());
        bytes[1] = 9;
        assert_eq!(
            parse_message(&RxFrame::from_bytes(&bytes)),
            Err(ParseError::UnknownMessageType)
        );
        bytes = encode_new_order(order());
        bytes[44] = 9;
        assert_eq!(
            parse_message(&RxFrame::from_bytes(&bytes)),
            Err(ParseError::InvalidSide)
        );
    }

    #[test]
    fn rejects_trailing_bytes_and_false_length() {
        let bytes = encode_new_order(order());
        let mut trailing = [0_u8; NEW_ORDER_LEN + 1];
        trailing[..NEW_ORDER_LEN].copy_from_slice(&bytes);
        assert_eq!(
            parse_message(&RxFrame::from_bytes(&trailing)),
            Err(ParseError::InvalidLength)
        );
        let mut false_length = bytes;
        false_length[3] = 44;
        assert_eq!(
            parse_message(&RxFrame::from_bytes(&false_length)),
            Err(ParseError::InvalidLength)
        );
    }

    #[test]
    fn malformed_input_smoke_never_panics() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut bytes = [0_u8; NEW_ORDER_LEN + 8];
        for _ in 0..50_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let length = usize::from(state.to_ne_bytes()[0]) % bytes.len();
            for (index, byte) in bytes[..length].iter_mut().enumerate() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = state.to_ne_bytes()[index & 7];
            }
            let frame = RxFrame::from_bytes(&bytes[..length]);
            let _ = parse_message(&frame);
        }
    }
}
