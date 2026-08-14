## Summary

## Ownership and capacity behavior

## Safety / unsafe audit

## Allocation and copy audit

## Correctness evidence

## Performance evidence and environment

## Quality gates

- [ ] `cargo fmt --all --check`
- [ ] `cargo check --workspace --all-targets --all-features`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace --all-features`
- [ ] `cargo test --doc --workspace`
- [ ] Loom/parser smoke/allocation/source-ratio gates

## Known limitations
