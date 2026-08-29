FROM rust:1.96.0-bookworm

RUN apt-get update \
    && apt-get install -y --no-install-recommends clang linux-perf numactl util-linux \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add clippy rustfmt

WORKDIR /workspace
COPY . .

RUN cargo fmt --all --check \
    && cargo check --workspace --all-targets --all-features \
    && cargo clippy --workspace --all-targets --all-features -- -D warnings \
    && cargo test --workspace --all-features

CMD ["cargo", "test", "--workspace"]
