# Demonstration Protocol

This binary protocol exercises parsing, sequencing, risk checks, and matching.
It does not implement FIX, OUCH, ITCH, SBE, or a certified venue protocol.

All multibyte integers use network byte order (big endian). Each frame must
contain exactly one message. Its length must match both the header and the
fixed length for its message type. Truncated messages and trailing bytes are
rejected.

## Common Header

| Offset | Width | Field | Value |
| ---: | ---: | --- | --- |
| 0 | 1 | Version | `2` |
| 1 | 1 | Message type | `1` new, `2` cancel, `3` replace |
| 2 | 2 | Total message length | Big-endian `u16` |

## New Order (`type = 1`, 46 bytes)

| Offset | Width | Field | Type |
| ---: | ---: | --- | --- |
| 4 | 8 | Order ID | `u64` |
| 12 | 4 | Account ID | `u32` |
| 16 | 4 | Instrument ID | `u32` |
| 20 | 8 | Price ticks | `i64` |
| 28 | 8 | Quantity | `u64` |
| 36 | 8 | Session sequence | `u64` |
| 44 | 1 | Side | `1` buy, `2` sell |
| 45 | 1 | Time in force | `1` GTC, `2` IOC, `3` FOK, `4` post-only |

Unknown side and time-in-force values are parse errors. Price and quantity
validation occurs after parsing, in the gateway's business checks.

- GTC matches eligible orders and rests any remainder.
- IOC matches eligible orders and discards any remainder.
- FOK fills the entire quantity within the price limit or rejects without
  changing the book. Insufficient report capacity also causes rejection.
- Post-only rejects if the order would trade. An accepted order joins the
  FIFO tail at its price.

## Cancel (`type = 2`, 28 bytes)

| Offset | Width | Field | Type |
| ---: | ---: | --- | --- |
| 4 | 8 | Order ID | `u64` |
| 12 | 4 | Account ID | `u32` |
| 16 | 4 | Instrument ID | `u32` |
| 20 | 8 | Session sequence | `u64` |

## Replace (`type = 3`, 44 bytes)

| Offset | Width | Field | Type |
| ---: | ---: | --- | --- |
| 4 | 8 | Order ID | `u64` |
| 12 | 4 | Account ID | `u32` |
| 16 | 4 | Instrument ID | `u32` |
| 20 | 8 | Session sequence | `u64` |
| 28 | 8 | Price ticks | `i64` |
| 36 | 8 | Quantity | `u64` |

A replace changes a resting order's price and remaining quantity. It retains
the order ID, account, instrument, and side. Quantity is the new remaining
quantity and must be positive.

Only a strict quantity reduction at the same price keeps FIFO priority. Every
other accepted replace moves the order to the tail at its destination price,
including a request with unchanged price and quantity. A price change that
would cross the opposite side rejects without changing the book. If the book
rejects after risk adjusts the reservation, the gateway restores the prior
reservation.

## Sequencing and ownership

The field named session sequence is the command sequence of one instrument's
gateway. Each gateway maintains its own counter. The multi-instrument router
passes this value through to the selected shard. Connection lifecycle and
retransmission state in `hft-session` are separate from this wire layout.

- A new gateway starts at sequence 1 and requires the exact next value. A
  restored gateway resumes its validated saved sequence.
- Parse errors, duplicate sequences, and gaps leave the gateway sequence
  unchanged. Sequence `u64::MAX` also rejects because it has no representable
  successor.
- Once parsing and sequence validation succeed, the gateway consumes the
  sequence before checking the order ID, risk limits, or book. Business
  rejections therefore consume a sequence and can be replayed deterministically.
- New order IDs must strictly exceed the gateway's highest received new order
  ID. The gateway records each higher ID before risk and book checks, so a
  business rejection still prevents reuse. This rule uses a single watermark.
- Cancel and replace requests must identify the original account and
  instrument. Ownership or instrument rejections preserve book and risk state,
  but consume an otherwise valid gateway sequence.

These rules describe direct gateway application. The router rejects unknown
instruments before queue publication. The journaled engine rejects instrument
mismatches and queue pressure before calling the gateway, without consuming
sequence. Neither entry point supplies connection authentication.
