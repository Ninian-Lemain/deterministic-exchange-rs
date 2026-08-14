# Architecture

## Data Path

1. An RX queue grants a frame lease borrowing driver-owned or preallocated
   memory. Dropping the lease permits descriptor/buffer recycling.
2. `hft-wire` validates version, type, declared and actual length, boundaries,
   endianness, and side before constructing a borrowed message.
3. The gateway requires the exact next session sequence before state mutation.
   A gap or duplicate fails closed and does not advance the expected sequence.
4. The gateway normalizes validated scalar fields once into a 48-byte
   `NewOrder`. Across cores this value belongs in a preallocated SPSC slot; this
   is a bounded single-copy handoff.
5. A matching shard checks and reserves account exposure, then mutates its
   single-writer book.
6. Execution reports are written into a caller-owned fixed report buffer. The
   gateway converts filled reservations into settled positions.
7. Owner-authorized cancellation uses a fixed-capacity `OrderId` index, removes
   the resting remainder without disturbing peer FIFO, and releases exactly
   that remaining risk reservation.
8. Replay hashes stable logical state, independent of array slot placement.

## Ownership

- A frame lease keeps its RX buffer borrowed; it cannot outlive the queue borrow.
- A gateway owns one initially empty book and one corresponding risk engine.
- An order book has one writer and no shared mutable access.
- Each book owns its open-addressed order index. Index slots use deterministic
  linear probing, remain at or below 50% live load, and never grow on the heap.
- `SpscQueue::split` requires an exclusive queue borrow and yields exactly one
  producer and consumer.
- Vendor sessions uniquely own one opaque handle and destroy it once.

## Capacity and Backpressure

- RX memory backend: `QueueError::Full`.
- SPSC: returns the original value when full.
- Risk accounts and orders: explicit account/order capacity rejection.
- Book: explicit price-level, per-level FIFO, and report capacity rejection.
- Order index: fixed at four slots per configured per-side order capacity;
  deletions compact probe clusters in place.
- No structure silently overwrites unread or live data.

## Cold and Hot Cores

Configuration, filesystem access, formatting, logging, metrics export, socket
setup, memory registration, affinity, and shutdown coordination belong off the
hot core. The implemented gateway method contains no such work.
