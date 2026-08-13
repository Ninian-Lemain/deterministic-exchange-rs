# Contributing

Keep hot-path changes deterministic, bounded, and reviewable.

1. Explain the ownership and capacity behavior of new state.
2. Keep business logic in safe Rust. Any new unsafe block needs a precise
   `SAFETY:` argument and a regression test.
3. Do not add allocation, locks, syscalls, logging, or formatting to the
   steady-state gateway path.
4. Add boundary and failure tests before performance tuning.
5. Run every command in [QUICKSTART.md](QUICKSTART.md).

Performance changes must include hardware, kernel, compiler, affinity, NUMA,
governor, warm-up, sample count, build flags, and before/after results. Hosted
runner timings are informational only.
