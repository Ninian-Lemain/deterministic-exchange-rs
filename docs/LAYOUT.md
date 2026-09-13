# Layout measurements

These measurements compare specific builds on x86_64 Windows with Rust 1.96.0,
LLVM 22.1.2, and an AMD Ryzen 7 7735HS. Release builds used fat LTO and one
codegen unit. Sizes depend on the target and configured capacities.

## Book and risk storage, 2026-09-10

| Type | Before bytes | After bytes |
| --- | ---: | ---: |
| Resting order slot | 64 | 56 |
| `OrderBook<1,64>` | 16,552 | 15,528 |
| `OrderBook<1,512>` | 131,240 | 123,048 |
| Risk index entry | 24 | 16 |
| `RiskEngine<64,1024>` | 93,216 | 75,808 |

Book commit `e17f78a` removed the price stored in each resting order. Export and
replace read it from the containing level. The change saved 16 bytes per
configured level/order pair across both sides.

Risk commit `c3d5efd` stored each private slot handle as `slot + 1` in a
`NonZeroUsize`. Zero represents an empty index entry. Account and order keys
keep their full ranges. Each index has two entries per configured slot, so
the change saved 16 bytes per configured account or reservation.

The changes preserved wire, journal, snapshot, and public C layouts. Model,
FIFO, replace, collision, slot reuse, and snapshot fixture tests passed for
these builds.

## Desktop comparison

[Raw results](evidence/layout-2026-09-10.zip) contain five paired book runs and
ten paired risk runs. Each run contains 103 benchmark cells. Every pair has
matching checksums, allocation counts, and deallocation counts. Recovery cells
allocate on their cold paths. Hot-path records report zero allocations, with
the session measurement gap described in
[Performance evidence](PERFORMANCE.md#allocation-policy).

The book comparison uses `a4098bd` as its baseline. The risk comparison uses
`e17f78a`, which already contains the book change. Risk runs 1 through 5 execute
the baseline first. Runs 6 through 10 reverse that order.

Risk comparison values below are medians of ten per-run means, in nanoseconds.
The range covers the lowest and highest per-run mean.

| Workload | Before median | After median | Before range | After range |
| --- | ---: | ---: | --- | --- |
| Gateway rest/fill, per message | 92.5 | 103 | 81 to 118 | 74 to 152 |
| Seeded gateway mix | 112.5 | 107 | 100 to 231 | 96 to 129 |
| Route, process, retrieve event | 109.5 | 118 | 101 to 211 | 107 to 226 |

The timing changes are mixed and the ranges overlap. Dedicated Linux runs are
needed before performance qualification.

## Book totals and route lookup

The 2026-09-12 comparison uses baseline `011ca7b` with benchmark changes from
`1dca1b5` and candidate `795908a`. The candidate stores each order location as
a nonzero flat handle encoding the side, level, and FIFO slot. Zero marks an
empty index entry. Order IDs still use all 64 bits.

Each price level maintains a `u128` quantity total. Insert, partial fill,
unlink, and replacement update it. Restore rebuilds it from logical orders.
The total adds 24 bytes per level on this target, including alignment. The
smaller order index saves more than that cost for the measured shapes.

| Type | Before bytes | After bytes |
| --- | ---: | ---: |
| Book order index entry | 32 | 16 |
| `OrderBook<1,8>` | 2,088 | 1,632 |
| `OrderBook<1,64>` | 15,528 | 11,488 |
| `OrderBook<1,512>` | 123,048 | 90,336 |
| `RouteTable<64>` | 768 | 644 |
| `RouteTable<1024>` | 12,288 | 10,244 |

The route table stores `u16` reverse indexes in place of duplicate instrument
IDs. A flag set at construction selects direct offsets for contiguous IDs and
binary search for sparse IDs. Reverse lookup adds an indexed read. Full
instrument IDs and all 65,536 shard IDs remain supported.

The sorted price directory stays contiguous. Inserting or removing prices
does not move order slots. These changes add no matching-path heap allocation
or logging and preserve wire, journal, snapshot, and C layouts.

[Measurements](PERFORMANCE.md#book-totals-and-direct-routing) include update
costs and sparse lookup regressions.
