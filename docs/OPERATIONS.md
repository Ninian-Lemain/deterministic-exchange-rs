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

`hft-recovery` restores a versioned snapshot and a contiguous journal tail.
The snapshot stores logical gateway, risk, and book state with its capacity
shape, applied sequence, and SHA-256 digest. Restore rejects corrupt,
truncated, unsupported, noncanonical, or capacity-incompatible state. Tail
replay rejects overlap, gaps, partial records, corrupt records, and a mismatch
between the journal sequence and wire payload sequence.

Snapshot publication accepts only a new generation path. It syncs the file and,
on Unix, its parent directory. The API distinguishes failure before publication
from failure after the destination became visible. The repository does not yet
provide generation naming, manifest replacement, snapshot retention, or
automatic selection of the latest valid snapshot. An adapter must stop order
admission, select an authoritative snapshot and tail, restore them, verify the
result, and only then reopen the shard.

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
