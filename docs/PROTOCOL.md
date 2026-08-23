# Demonstration Protocol

This small binary protocol exists to exercise parser, session, risk, and
matching behavior. It is not presented as FIX, OUCH, ITCH, SBE, or a certified
venue protocol.

All multibyte integers use network byte order (big endian). Messages must match
the declared length exactly; trailing bytes are rejected.

## Common Header

| Offset | Width | Field | Value |
| ---: | ---: | --- | --- |
| 0 | 1 | Version | `2` |
| 1 | 1 | Message type | `1` new, `2` cancel |
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

## Cancel (`type = 2`, 28 bytes)

| Offset | Width | Field | Type |
| ---: | ---: | --- | --- |
| 4 | 8 | Order ID | `u64` |
| 12 | 4 | Account ID | `u32` |
| 16 | 4 | Instrument ID | `u32` |
| 20 | 8 | Session sequence | `u64` |

## Session Semantics

- A gateway starts at sequence 1 and requires the exact next value.
- Duplicate and gap messages fail closed without advancing the session.
- A syntactically valid message consumes its sequence before business checks;
  a business reject is therefore deterministic and replayable.
- New order IDs must increase monotonically for the gateway session, including
  business-rejected requests. This prevents reuse with bounded state.
- A cancel must name the original account and instrument. Unauthorized cancel
  attempts do not modify book or risk state.
