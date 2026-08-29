//! Versioned snapshots and snapshot plus journal-tail recovery.
#![forbid(unsafe_code)]

use hft_book::{BookLevelState, BookState};
use hft_gateway::{Gateway, GatewayError, GatewayState, GatewayStateError};
use hft_io::RxFrame;
use hft_journal::{DecodeError, JournalRecord, RECORD_SIZE};
use hft_risk::{AccountRiskState, ReservationRiskState, RiskEngineState, RiskLimits};
use hft_types::{
    AccountId, InstrumentId, OrderId, PriceTicks, Quantity, ReportBuffer, SequenceNumber, Side,
};
use hft_wire::{ParseError, parse_message};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub const FORMAT_VERSION: u16 = 1;
pub const HEADER_SIZE: usize = 96;
const MAGIC: [u8; 8] = *b"HFTSNAP\0";
const HASH_ALGORITHM_SHA256: u16 = 1;
const DIGEST_OFFSET: usize = 52;
const DIGEST_SIZE: usize = 32;
const DOMAIN: &[u8] = b"deterministic-exchange snapshot v1\0";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    bytes: Vec<u8>,
    digest: [u8; DIGEST_SIZE],
    applied_sequence: u64,
}

impl Snapshot {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn digest(&self) -> [u8; DIGEST_SIZE] {
        self.digest
    }

    #[must_use]
    pub const fn applied_sequence(&self) -> u64 {
        self.applied_sequence
    }
}

#[derive(Debug)]
pub struct DecodedSnapshot<
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS_PER_LEVEL: usize,
> {
    pub gateway: Gateway<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>,
    pub applied_sequence: u64,
    pub digest: [u8; DIGEST_SIZE],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotError {
    Truncated,
    TrailingBytes,
    BadMagic,
    UnsupportedVersion { found: u16 },
    UnsupportedFlags { found: u32 },
    UnsupportedHash { found: u16 },
    HeaderLength { found: u16 },
    DigestLength { found: u16 },
    LengthOverflow,
    CapacityMismatch,
    IntegrityMismatch,
    InvalidBoolean,
    InvalidSide,
    InvalidCount,
    InvalidSequence,
    NonCanonical,
    GatewayState(GatewayStateError),
}

#[derive(Debug)]
pub enum RecoveryError {
    Snapshot(SnapshotError),
    TruncatedJournal { bytes: usize },
    JournalDecode { offset: usize, source: DecodeError },
    JournalSequence { expected: u64, received: u64 },
    PayloadSequence { journal: u64, payload: u64 },
    PayloadParse(ParseError),
    Gateway(GatewayError),
    SequenceOverflow,
}

#[derive(Debug)]
pub enum SnapshotPersistError {
    BeforePublication(io::Error),
    Published(io::Error),
}

/// Encodes one gateway state in canonical logical order.
///
/// # Errors
///
/// Returns an error when the supplied journal cut does not match the next
/// gateway sequence or when a length cannot be represented by the format.
pub fn encode_snapshot<
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS_PER_LEVEL: usize,
>(
    gateway: &Gateway<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>,
    applied_sequence: u64,
) -> Result<Snapshot, SnapshotError> {
    let state = gateway.export_state();
    if state.expected_sequence.0
        != applied_sequence
            .checked_add(1)
            .ok_or(SnapshotError::InvalidSequence)?
    {
        return Err(SnapshotError::InvalidSequence);
    }
    let mut payload = Vec::new();
    encode_gateway_state(&state, &mut payload)?;
    let payload_len = u64::try_from(payload.len()).map_err(|_| SnapshotError::LengthOverflow)?;
    let mut header = [0_u8; HEADER_SIZE];
    header[0..8].copy_from_slice(&MAGIC);
    header[8..10].copy_from_slice(&FORMAT_VERSION.to_be_bytes());
    let header_size = u16::try_from(HEADER_SIZE).map_err(|_| SnapshotError::LengthOverflow)?;
    header[10..12].copy_from_slice(&header_size.to_be_bytes());
    header[16..18].copy_from_slice(&HASH_ALGORITHM_SHA256.to_be_bytes());
    let digest_size = u16::try_from(DIGEST_SIZE).map_err(|_| SnapshotError::LengthOverflow)?;
    header[18..20].copy_from_slice(&digest_size.to_be_bytes());
    header[20..28].copy_from_slice(&payload_len.to_be_bytes());
    header[28..36].copy_from_slice(&applied_sequence.to_be_bytes());
    write_capacity::<ACCOUNTS>(&mut header[36..40])?;
    write_capacity::<RISK_ORDERS>(&mut header[40..44])?;
    write_capacity::<LEVELS>(&mut header[44..48])?;
    write_capacity::<ORDERS_PER_LEVEL>(&mut header[48..52])?;
    let digest = snapshot_digest(&header, &payload);
    header[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_SIZE].copy_from_slice(&digest);
    let mut bytes = Vec::with_capacity(HEADER_SIZE + payload.len());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(&payload);
    Ok(Snapshot {
        bytes,
        digest,
        applied_sequence,
    })
}

/// Verifies and restores a canonical snapshot.
///
/// # Errors
///
/// Returns the first container, integrity, capacity, encoding, or logical
/// state error. No live gateway is mutated on failure.
pub fn decode_snapshot<
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS_PER_LEVEL: usize,
>(
    bytes: &[u8],
) -> Result<DecodedSnapshot<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>, SnapshotError> {
    if bytes.len() < HEADER_SIZE {
        return Err(SnapshotError::Truncated);
    }
    let header: &[u8; HEADER_SIZE] = bytes[..HEADER_SIZE]
        .try_into()
        .map_err(|_| SnapshotError::Truncated)?;
    if header[0..8] != MAGIC {
        return Err(SnapshotError::BadMagic);
    }
    let version = read_u16(&header[8..10])?;
    if version != FORMAT_VERSION {
        return Err(SnapshotError::UnsupportedVersion { found: version });
    }
    let header_len = read_u16(&header[10..12])?;
    if usize::from(header_len) != HEADER_SIZE {
        return Err(SnapshotError::HeaderLength { found: header_len });
    }
    let flags = read_u32(&header[12..16])?;
    if flags != 0 {
        return Err(SnapshotError::UnsupportedFlags { found: flags });
    }
    let hash = read_u16(&header[16..18])?;
    if hash != HASH_ALGORITHM_SHA256 {
        return Err(SnapshotError::UnsupportedHash { found: hash });
    }
    let digest_len = read_u16(&header[18..20])?;
    if usize::from(digest_len) != DIGEST_SIZE {
        return Err(SnapshotError::DigestLength { found: digest_len });
    }
    let payload_len =
        usize::try_from(read_u64(&header[20..28])?).map_err(|_| SnapshotError::LengthOverflow)?;
    let total = HEADER_SIZE
        .checked_add(payload_len)
        .ok_or(SnapshotError::LengthOverflow)?;
    match bytes.len().cmp(&total) {
        core::cmp::Ordering::Less => return Err(SnapshotError::Truncated),
        core::cmp::Ordering::Greater => return Err(SnapshotError::TrailingBytes),
        core::cmp::Ordering::Equal => {}
    }
    if read_u32(&header[36..40])? != capacity::<ACCOUNTS>()?
        || read_u32(&header[40..44])? != capacity::<RISK_ORDERS>()?
        || read_u32(&header[44..48])? != capacity::<LEVELS>()?
        || read_u32(&header[48..52])? != capacity::<ORDERS_PER_LEVEL>()?
    {
        return Err(SnapshotError::CapacityMismatch);
    }
    if header[84..].iter().any(|byte| *byte != 0) {
        return Err(SnapshotError::NonCanonical);
    }
    let payload = &bytes[HEADER_SIZE..];
    let expected_digest = snapshot_digest(header, payload);
    let stored_digest: [u8; DIGEST_SIZE] = header[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_SIZE]
        .try_into()
        .map_err(|_| SnapshotError::Truncated)?;
    if expected_digest != stored_digest {
        return Err(SnapshotError::IntegrityMismatch);
    }
    let applied_sequence = read_u64(&header[28..36])?;
    let state = decode_gateway_state::<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>(payload)?;
    if state.expected_sequence.0
        != applied_sequence
            .checked_add(1)
            .ok_or(SnapshotError::InvalidSequence)?
    {
        return Err(SnapshotError::InvalidSequence);
    }
    let gateway = Gateway::from_state(&state).map_err(SnapshotError::GatewayState)?;
    Ok(DecodedSnapshot {
        gateway,
        applied_sequence,
        digest: stored_digest,
    })
}

/// Restores a snapshot and applies complete ordered journal records after it.
/// Business rejections consume sequence and remain part of the recovered
/// command history. Parse, sequence, and internal state errors stop recovery.
///
/// # Errors
///
/// Returns the first snapshot, journal, payload, or gateway state error.
pub fn recover_snapshot_and_tail<
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS_PER_LEVEL: usize,
    const REPORTS: usize,
>(
    snapshot: &[u8],
    tail: &[u8],
) -> Result<Gateway<ACCOUNTS, RISK_ORDERS, LEVELS, ORDERS_PER_LEVEL>, RecoveryError> {
    let decoded = decode_snapshot(snapshot).map_err(RecoveryError::Snapshot)?;
    if tail.len() % RECORD_SIZE != 0 {
        return Err(RecoveryError::TruncatedJournal {
            bytes: tail.len() % RECORD_SIZE,
        });
    }
    let mut gateway = decoded.gateway;
    let mut expected = decoded
        .applied_sequence
        .checked_add(1)
        .ok_or(RecoveryError::SequenceOverflow)?;
    let mut reports = ReportBuffer::<REPORTS>::new();
    for (index, chunk) in tail.chunks_exact(RECORD_SIZE).enumerate() {
        let bytes: &[u8; RECORD_SIZE] = chunk
            .try_into()
            .map_err(|_| RecoveryError::TruncatedJournal { bytes: chunk.len() })?;
        let record =
            JournalRecord::decode(bytes).map_err(|source| RecoveryError::JournalDecode {
                offset: index * RECORD_SIZE,
                source,
            })?;
        if record.sequence().0 != expected {
            return Err(RecoveryError::JournalSequence {
                expected,
                received: record.sequence().0,
            });
        }
        let payload = record.slice().ok_or(RecoveryError::JournalDecode {
            offset: index * RECORD_SIZE,
            source: DecodeError::PayloadLength { found: u16::MAX },
        })?;
        let frame = RxFrame::from_bytes(payload);
        let message = parse_message(&frame).map_err(RecoveryError::PayloadParse)?;
        if message.sequence().0 != record.sequence().0 {
            return Err(RecoveryError::PayloadSequence {
                journal: record.sequence().0,
                payload: message.sequence().0,
            });
        }
        match gateway.process_frame(&frame, &mut reports) {
            Ok(_) | Err(GatewayError::Risk(_) | GatewayError::Book(_)) => {}
            Err(error) => return Err(RecoveryError::Gateway(error)),
        }
        expected = expected
            .checked_add(1)
            .ok_or(RecoveryError::SequenceOverflow)?;
    }
    Ok(gateway)
}

/// Writes and syncs a snapshot before publishing it at a new path.
///
/// The destination must not exist. Snapshot replacement needs a separate
/// generation or manifest policy so readers never observe an overwrite gap.
///
/// # Errors
///
/// Returns the first temporary-file, write, sync, publication, cleanup, or
/// directory-sync error. Errors report whether the destination was published.
pub fn persist_snapshot_new(path: &Path, snapshot: &Snapshot) -> Result<(), SnapshotPersistError> {
    if path.exists() {
        return Err(SnapshotPersistError::BeforePublication(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "snapshot destination already exists",
        )));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            SnapshotPersistError::BeforePublication(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid snapshot path",
            ))
        })?;
    let suffix = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".{file_name}.tmp-{}-{suffix}", std::process::id()));
    let before_publication = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(snapshot.bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::hard_link(&temporary, path)?;
        Ok(())
    })();
    if let Err(error) = before_publication {
        let _ = std::fs::remove_file(&temporary);
        return Err(SnapshotPersistError::BeforePublication(error));
    }
    if let Err(error) = std::fs::remove_file(&temporary) {
        return Err(SnapshotPersistError::Published(error));
    }
    #[cfg(unix)]
    if let Err(error) = std::fs::File::open(parent).and_then(|directory| directory.sync_all()) {
        return Err(SnapshotPersistError::Published(error));
    }
    Ok(())
}

fn snapshot_digest(header: &[u8; HEADER_SIZE], payload: &[u8]) -> [u8; DIGEST_SIZE] {
    let mut canonical_header = *header;
    canonical_header[DIGEST_OFFSET..DIGEST_OFFSET + DIGEST_SIZE].fill(0);
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hasher.update(canonical_header);
    hasher.update(payload);
    hasher.finalize().into()
}

fn encode_gateway_state<const LEVELS: usize, const ORDERS: usize>(
    state: &GatewayState<LEVELS, ORDERS>,
    out: &mut Vec<u8>,
) -> Result<(), SnapshotError> {
    out.extend_from_slice(&state.instrument.0.to_be_bytes());
    out.extend_from_slice(&state.expected_sequence.0.to_be_bytes());
    encode_optional_order(state.maximum_received_order_id, out);
    encode_optional_order(state.risk.maximum_order_id, out);
    out.push(u8::from(state.risk.killed));
    write_count(state.risk.accounts.len(), out)?;
    write_count(state.risk.reservations.len(), out)?;
    write_count(state.book.bid_level_count, out)?;
    write_count(state.book.ask_level_count, out)?;
    for account in &state.risk.accounts {
        encode_account(account, out);
    }
    for reservation in &state.risk.reservations {
        encode_reservation(reservation, out);
    }
    encode_levels(&state.book.bids[..state.book.bid_level_count], out)?;
    encode_levels(&state.book.asks[..state.book.ask_level_count], out)?;
    Ok(())
}

fn encode_account(account: &AccountRiskState, out: &mut Vec<u8>) {
    out.extend_from_slice(&account.id.0.to_be_bytes());
    out.extend_from_slice(&account.limits.max_quantity.0.to_be_bytes());
    out.extend_from_slice(&account.limits.max_notional.to_be_bytes());
    out.extend_from_slice(&account.limits.max_abs_position.0.to_be_bytes());
    out.extend_from_slice(&account.limits.max_open_orders.to_be_bytes());
    out.extend_from_slice(&account.limits.minimum_price.0.to_be_bytes());
    out.extend_from_slice(&account.limits.maximum_price.0.to_be_bytes());
    out.extend_from_slice(&account.settled_position.to_be_bytes());
    out.extend_from_slice(&account.reserved_buys.to_be_bytes());
    out.extend_from_slice(&account.reserved_sells.to_be_bytes());
    out.extend_from_slice(&account.open_orders.to_be_bytes());
    out.push(u8::from(account.killed));
}

fn encode_reservation(reservation: &ReservationRiskState, out: &mut Vec<u8>) {
    out.extend_from_slice(&reservation.order_id.0.to_be_bytes());
    out.extend_from_slice(&reservation.account_id.0.to_be_bytes());
    out.push(side_tag(reservation.side));
    out.extend_from_slice(&reservation.quantity.0.to_be_bytes());
}

fn encode_levels<const ORDERS: usize>(
    levels: &[BookLevelState<ORDERS>],
    out: &mut Vec<u8>,
) -> Result<(), SnapshotError> {
    for level in levels {
        out.push(side_tag(level.side));
        out.extend_from_slice(&level.price.0.to_be_bytes());
        write_count(level.order_count, out)?;
        for order in &level.orders[..level.order_count] {
            out.extend_from_slice(&order.order_id.0.to_be_bytes());
            out.extend_from_slice(&order.account_id.0.to_be_bytes());
            out.push(side_tag(order.side));
            out.extend_from_slice(&order.price.0.to_be_bytes());
            out.extend_from_slice(&order.quantity.0.to_be_bytes());
            out.extend_from_slice(&order.sequence.0.to_be_bytes());
        }
    }
    Ok(())
}

fn decode_gateway_state<
    const ACCOUNTS: usize,
    const RISK_ORDERS: usize,
    const LEVELS: usize,
    const ORDERS: usize,
>(
    payload: &[u8],
) -> Result<GatewayState<LEVELS, ORDERS>, SnapshotError> {
    let mut input = Input::new(payload);
    let instrument = InstrumentId(input.u32()?);
    let expected_sequence = SequenceNumber(input.u64()?);
    let maximum_received_order_id = input.optional_order()?;
    let risk_maximum_order_id = input.optional_order()?;
    let killed = input.boolean()?;
    let account_count = input.count()?;
    let reservation_count = input.count()?;
    let bid_count = input.count()?;
    let ask_count = input.count()?;
    if account_count > ACCOUNTS
        || reservation_count > RISK_ORDERS
        || bid_count > LEVELS
        || ask_count > LEVELS
    {
        return Err(SnapshotError::InvalidCount);
    }
    let mut accounts = Vec::with_capacity(account_count);
    for _ in 0..account_count {
        accounts.push(input.account()?);
    }
    let mut reservations = Vec::with_capacity(reservation_count);
    for _ in 0..reservation_count {
        reservations.push(input.reservation()?);
    }
    let mut book = BookState::empty(instrument);
    book.bid_level_count = bid_count;
    for level in &mut book.bids[..bid_count] {
        *level = input.level::<ORDERS>()?;
    }
    book.ask_level_count = ask_count;
    for level in &mut book.asks[..ask_count] {
        *level = input.level::<ORDERS>()?;
    }
    if !input.is_empty() {
        return Err(SnapshotError::TrailingBytes);
    }
    Ok(GatewayState {
        instrument,
        book,
        risk: RiskEngineState {
            accounts,
            reservations,
            maximum_order_id: risk_maximum_order_id,
            killed,
        },
        expected_sequence,
        maximum_received_order_id,
    })
}

struct Input<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Input<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(SnapshotError::LengthOverflow)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(SnapshotError::Truncated)?;
        self.offset = end;
        Ok(bytes)
    }
    fn u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, SnapshotError> {
        read_u32(self.take(4)?)
    }
    fn u64(&mut self) -> Result<u64, SnapshotError> {
        read_u64(self.take(8)?)
    }
    fn u128(&mut self) -> Result<u128, SnapshotError> {
        Ok(u128::from_be_bytes(
            self.take(16)?
                .try_into()
                .map_err(|_| SnapshotError::Truncated)?,
        ))
    }
    fn i64(&mut self) -> Result<i64, SnapshotError> {
        Ok(i64::from_be_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| SnapshotError::Truncated)?,
        ))
    }
    fn i128(&mut self) -> Result<i128, SnapshotError> {
        Ok(i128::from_be_bytes(
            self.take(16)?
                .try_into()
                .map_err(|_| SnapshotError::Truncated)?,
        ))
    }
    fn boolean(&mut self) -> Result<bool, SnapshotError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotError::InvalidBoolean),
        }
    }
    fn side(&mut self) -> Result<Side, SnapshotError> {
        match self.u8()? {
            1 => Ok(Side::Buy),
            2 => Ok(Side::Sell),
            _ => Err(SnapshotError::InvalidSide),
        }
    }
    fn count(&mut self) -> Result<usize, SnapshotError> {
        usize::try_from(self.u32()?).map_err(|_| SnapshotError::InvalidCount)
    }
    fn optional_order(&mut self) -> Result<Option<OrderId>, SnapshotError> {
        let present = self.boolean()?;
        let value = self.u64()?;
        if present {
            Ok(Some(OrderId(value)))
        } else if value == 0 {
            Ok(None)
        } else {
            Err(SnapshotError::NonCanonical)
        }
    }
    fn account(&mut self) -> Result<AccountRiskState, SnapshotError> {
        Ok(AccountRiskState {
            id: AccountId(self.u32()?),
            limits: RiskLimits {
                max_quantity: Quantity(self.u64()?),
                max_notional: self.u128()?,
                max_abs_position: Quantity(self.u64()?),
                max_open_orders: self.u32()?,
                minimum_price: PriceTicks(self.i64()?),
                maximum_price: PriceTicks(self.i64()?),
            },
            settled_position: self.i128()?,
            reserved_buys: self.u128()?,
            reserved_sells: self.u128()?,
            open_orders: self.u32()?,
            killed: self.boolean()?,
        })
    }
    fn reservation(&mut self) -> Result<ReservationRiskState, SnapshotError> {
        Ok(ReservationRiskState {
            order_id: OrderId(self.u64()?),
            account_id: AccountId(self.u32()?),
            side: self.side()?,
            quantity: Quantity(self.u64()?),
        })
    }
    fn level<const ORDERS: usize>(&mut self) -> Result<BookLevelState<ORDERS>, SnapshotError> {
        let side = self.side()?;
        let price = PriceTicks(self.i64()?);
        let count = self.count()?;
        if count > ORDERS {
            return Err(SnapshotError::InvalidCount);
        }
        let mut level = BookLevelState::empty(side, price);
        level.order_count = count;
        for order in &mut level.orders[..count] {
            *order = hft_book::BookOrderState {
                order_id: OrderId(self.u64()?),
                account_id: AccountId(self.u32()?),
                side: self.side()?,
                price: PriceTicks(self.i64()?),
                quantity: Quantity(self.u64()?),
                sequence: SequenceNumber(self.u64()?),
            };
        }
        Ok(level)
    }
    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn encode_optional_order(value: Option<OrderId>, out: &mut Vec<u8>) {
    out.push(u8::from(value.is_some()));
    out.extend_from_slice(&value.map_or(0, |id| id.0).to_be_bytes());
}
fn side_tag(side: Side) -> u8 {
    match side {
        Side::Buy => 1,
        Side::Sell => 2,
    }
}
fn write_count(value: usize, out: &mut Vec<u8>) -> Result<(), SnapshotError> {
    out.extend_from_slice(
        &u32::try_from(value)
            .map_err(|_| SnapshotError::LengthOverflow)?
            .to_be_bytes(),
    );
    Ok(())
}
fn write_capacity<const N: usize>(out: &mut [u8]) -> Result<(), SnapshotError> {
    out.copy_from_slice(&capacity::<N>()?.to_be_bytes());
    Ok(())
}
fn capacity<const N: usize>() -> Result<u32, SnapshotError> {
    u32::try_from(N).map_err(|_| SnapshotError::LengthOverflow)
}
fn read_u16(bytes: &[u8]) -> Result<u16, SnapshotError> {
    Ok(u16::from_be_bytes(
        bytes.try_into().map_err(|_| SnapshotError::Truncated)?,
    ))
}
fn read_u32(bytes: &[u8]) -> Result<u32, SnapshotError> {
    Ok(u32::from_be_bytes(
        bytes.try_into().map_err(|_| SnapshotError::Truncated)?,
    ))
}
fn read_u64(bytes: &[u8]) -> Result<u64, SnapshotError> {
    Ok(u64::from_be_bytes(
        bytes.try_into().map_err(|_| SnapshotError::Truncated)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_risk::RiskEngine;
    use hft_types::{NewOrder, RejectReason, TimeInForce};
    use hft_wire::encode_new_order;

    type TestGateway = Gateway<2, 8, 4, 4>;

    fn gateway() -> TestGateway {
        let mut risk = RiskEngine::new();
        let limits = RiskLimits {
            max_quantity: Quantity(10),
            max_notional: 10_000,
            max_abs_position: Quantity(100),
            max_open_orders: 8,
            minimum_price: PriceTicks(1),
            maximum_price: PriceTicks(1_000),
        };
        risk.register_account(AccountId(1), limits)
            .expect("account one");
        risk.register_account(AccountId(2), limits)
            .expect("account two");
        Gateway::new(risk, InstrumentId(7))
    }

    fn order(id: u64, account: u32, side: Side, quantity: u64, sequence: u64) -> [u8; 46] {
        encode_new_order(NewOrder {
            order_id: OrderId(id),
            account_id: AccountId(account),
            instrument_id: InstrumentId(7),
            price: PriceTicks(100),
            quantity: Quantity(quantity),
            sequence: SequenceNumber(sequence),
            side,
            time_in_force: TimeInForce::Gtc,
        })
    }

    fn apply(gateway: &mut TestGateway, bytes: &[u8]) -> Result<(), GatewayError> {
        let mut reports = ReportBuffer::<4>::new();
        match gateway.process_frame(&RxFrame::from_bytes(bytes), &mut reports) {
            Ok(_) | Err(GatewayError::Risk(_) | GatewayError::Book(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    #[test]
    fn canonical_snapshot_round_trips_with_fixed_integrity_value() {
        let mut gateway = gateway();
        apply(&mut gateway, &order(1, 1, Side::Sell, 5, 1)).expect("first command");
        let snapshot = encode_snapshot(&gateway, 1).expect("snapshot");
        assert_eq!(snapshot.bytes(), fixture_bytes().as_slice());
        let decoded = decode_snapshot::<2, 8, 4, 4>(snapshot.bytes()).expect("decode");
        assert_eq!(decoded.gateway.export_state(), gateway.export_state());
        assert_eq!(
            encode_snapshot(&decoded.gateway, 1).expect("reencode"),
            snapshot
        );
        assert_eq!(
            snapshot.digest(),
            [
                117, 217, 50, 69, 103, 188, 147, 161, 214, 155, 50, 244, 177, 27, 55, 12, 8, 241,
                90, 209, 9, 97, 61, 245, 193, 128, 201, 217, 221, 211, 218, 5,
            ]
        );
    }

    #[test]
    fn container_corruption_version_truncation_and_trailing_bytes_fail_closed() {
        let snapshot = encode_snapshot(&gateway(), 0).expect("snapshot");
        for length in [0, 8, HEADER_SIZE - 1, snapshot.bytes().len() - 1] {
            assert!(matches!(
                decode_snapshot::<2, 8, 4, 4>(&snapshot.bytes()[..length]),
                Err(SnapshotError::Truncated)
            ));
        }

        let mut corrupt = snapshot.bytes().to_vec();
        corrupt[HEADER_SIZE] ^= 1;
        assert!(matches!(
            decode_snapshot::<2, 8, 4, 4>(&corrupt),
            Err(SnapshotError::IntegrityMismatch)
        ));

        let mut version = snapshot.bytes().to_vec();
        version[8..10].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(
            decode_snapshot::<2, 8, 4, 4>(&version).map(|_| ()),
            Err(SnapshotError::UnsupportedVersion { found: 2 })
        );

        let mut trailing = snapshot.bytes().to_vec();
        trailing.push(0);
        assert!(matches!(
            decode_snapshot::<2, 8, 4, 4>(&trailing),
            Err(SnapshotError::TrailingBytes)
        ));
        assert!(matches!(
            decode_snapshot::<3, 8, 4, 4>(snapshot.bytes()),
            Err(SnapshotError::CapacityMismatch)
        ));
    }

    #[test]
    fn snapshot_plus_rejected_and_accepted_tail_equals_full_replay() {
        let frames = [
            order(1, 1, Side::Sell, 5, 1),
            order(10, 1, Side::Sell, 11, 2),
            order(10, 1, Side::Sell, 5, 3),
            order(11, 2, Side::Buy, 5, 4),
        ];
        let mut full = gateway();
        for frame in &frames {
            apply(&mut full, frame).expect("full replay");
        }

        let mut prefix = gateway();
        apply(&mut prefix, &frames[0]).expect("prefix one");
        apply(&mut prefix, &frames[1]).expect("prefix rejection");
        assert_eq!(
            prefix.process_frame(
                &RxFrame::from_bytes(&frames[2]),
                &mut ReportBuffer::<4>::new()
            ),
            Err(GatewayError::Risk(RejectReason::DuplicateOrderId))
        );
        let snapshot = encode_snapshot(&prefix, 3).expect("snapshot after rejection");

        let record = JournalRecord::new(SequenceNumber(4), &frames[3])
            .expect("journal record")
            .encode();
        let recovered = recover_snapshot_and_tail::<2, 8, 4, 4, 4>(snapshot.bytes(), &record)
            .expect("snapshot plus tail");
        assert_eq!(recovered.export_state(), full.export_state());
        assert_eq!(
            encode_snapshot(&recovered, 4)
                .expect("recovered snapshot")
                .bytes(),
            encode_snapshot(&full, 4).expect("full snapshot").bytes()
        );
    }

    #[test]
    fn tail_overlap_gap_payload_mismatch_and_partial_record_fail_closed() {
        let snapshot = encode_snapshot(&gateway(), 0).expect("snapshot");
        let frame = order(1, 1, Side::Sell, 5, 1);
        let overlap = JournalRecord::new(SequenceNumber(0), &frame)
            .expect("record")
            .encode();
        assert!(matches!(
            recover_snapshot_and_tail::<2, 8, 4, 4, 4>(snapshot.bytes(), &overlap),
            Err(RecoveryError::JournalSequence { .. })
        ));
        let gap = JournalRecord::new(SequenceNumber(2), &frame)
            .expect("record")
            .encode();
        assert!(matches!(
            recover_snapshot_and_tail::<2, 8, 4, 4, 4>(snapshot.bytes(), &gap),
            Err(RecoveryError::JournalSequence { .. })
        ));
        let wrong_payload = JournalRecord::new(SequenceNumber(1), &order(1, 1, Side::Sell, 5, 2))
            .expect("record")
            .encode();
        assert!(matches!(
            recover_snapshot_and_tail::<2, 8, 4, 4, 4>(snapshot.bytes(), &wrong_payload),
            Err(RecoveryError::PayloadSequence { .. })
        ));
        assert!(matches!(
            recover_snapshot_and_tail::<2, 8, 4, 4, 4>(snapshot.bytes(), &gap[..13]),
            Err(RecoveryError::TruncatedJournal { bytes: 13 })
        ));
    }

    #[test]
    fn persisted_snapshot_reopens_and_refuses_overwrite() {
        let snapshot = encode_snapshot(&gateway(), 0).expect("snapshot");
        let mut path = std::env::temp_dir();
        path.push(format!(
            "hft-snapshot-{}-{}.bin",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        persist_snapshot_new(&path, &snapshot).expect("persist snapshot");
        let bytes = std::fs::read(&path).expect("read snapshot");
        let decoded = decode_snapshot::<2, 8, 4, 4>(&bytes).expect("decode persisted snapshot");
        assert_eq!(decoded.gateway.export_state(), gateway().export_state());
        assert!(matches!(
            persist_snapshot_new(&path, &snapshot),
            Err(SnapshotPersistError::BeforePublication(error))
                if error.kind() == io::ErrorKind::AlreadyExists
        ));
        std::fs::remove_file(path).expect("remove snapshot");
    }

    #[test]
    fn persisted_snapshot_and_journal_tail_reproduce_full_state_after_reopen() {
        let frames = [
            order(1, 1, Side::Sell, 5, 1),
            order(10, 1, Side::Sell, 11, 2),
            order(10, 1, Side::Sell, 5, 3),
            order(11, 2, Side::Buy, 5, 4),
        ];
        let mut full = gateway();
        for frame in &frames {
            apply(&mut full, frame).expect("full replay");
        }
        let mut prefix = gateway();
        apply(&mut prefix, &frames[0]).expect("prefix");
        let snapshot = encode_snapshot(&prefix, 1).expect("snapshot");

        let unique = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut snapshot_path = std::env::temp_dir();
        snapshot_path.push(format!(
            "hft-restart-{}-{unique}.snapshot",
            std::process::id()
        ));
        let mut journal_path = std::env::temp_dir();
        journal_path.push(format!(
            "hft-restart-{}-{unique}.journal",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&snapshot_path);
        let _ = std::fs::remove_file(&journal_path);
        persist_snapshot_new(&snapshot_path, &snapshot).expect("persist snapshot");

        let mut journal = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&journal_path)
            .expect("create journal");
        for (offset, frame) in frames[1..].iter().enumerate() {
            let sequence = u64::try_from(offset).expect("offset") + 2;
            journal
                .write_all(
                    &JournalRecord::new(SequenceNumber(sequence), frame)
                        .expect("record")
                        .encode(),
                )
                .expect("write journal");
        }
        journal.sync_all().expect("sync journal");
        drop(journal);

        let snapshot_bytes = std::fs::read(&snapshot_path).expect("reopen snapshot");
        let journal_bytes = std::fs::read(&journal_path).expect("reopen journal");
        let recovered = recover_snapshot_and_tail::<2, 8, 4, 4, 4>(&snapshot_bytes, &journal_bytes)
            .expect("recover after reopen");
        assert_eq!(recovered.export_state(), full.export_state());
        assert_eq!(
            encode_snapshot(&recovered, 4).expect("recovered bytes"),
            encode_snapshot(&full, 4).expect("full bytes")
        );

        std::fs::remove_file(snapshot_path).expect("remove snapshot");
        std::fs::remove_file(journal_path).expect("remove journal");
    }

    fn fixture_bytes() -> Vec<u8> {
        include_str!("../tests/fixtures/populated-v1.hex")
            .trim()
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text = core::str::from_utf8(pair).expect("fixture uses ASCII hex");
                u8::from_str_radix(text, 16).expect("fixture contains valid hex")
            })
            .collect()
    }
}
