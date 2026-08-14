# Operational Model

## Process Layout

The intended Linux deployment dedicates one single-writer shard to an
instrument group. Cold-path control configures sockets, memory, rings, affinity,
NUMA placement, logging, metrics export, and shutdown before the hot loop.
Inbound work crosses cores only as a normalized fixed-size SPSC value.

## Fail-Closed Conditions

- Invalid packet/version/type/length/side: parse rejection.
- Duplicate or missing sequence: session rejection; expected sequence unchanged.
- Risk arithmetic, limit, kill switch, account, order, or capacity failure:
  order rejection; risk and book unchanged.
- Report, level, or per-price FIFO exhaustion: book rejection after preflight;
  the gateway releases the taker reservation.
- Unauthorized/unknown cancel: rejection; book and risk unchanged.
- SPSC full: producer retains the value and observes explicit backpressure.
- RX ring full: backend rejects rather than overwriting an unread frame.

## Recovery Boundary

The current replay engine proves deterministic in-process state evolution but
does not yet provide durable journal/snapshot recovery or a retransmission
protocol. A production adapter must stop order admission on sequence loss,
obtain authoritative recovery data, replay it, verify the state digest, and only
then reopen the shard.

## Linux Qualification Checklist

- Pin shard and NIC queue IRQs to topology-aware isolated cores.
- Place UMEM, book, risk, SPSC, and TX frames on the NIC-local NUMA node.
- Prefault and lock memory; validate hugepage policy outside the hot loop.
- Exercise RX/TX exhaustion, link reset, process shutdown, and recovery.
- Record kernel, firmware, mitigations, governor, compiler flags, offered load,
  percentiles, queue occupancy, cache/branch misses, context switches, page
  faults, and allocation deltas.
- Never promote hosted-runner or Windows timing to a latency SLO.

## Integration Boundaries

- `UdpRx` is a portable syscall baseline.
- `af-xdp` is a truthful feature-gated marker until real descriptor/UMEM
  ownership is implemented on Linux.
- `VendorSession` is a safe ownership wrapper around an unavailable SDK, not a
  simulated vendor backend.
