FROM rust:1.95-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples ./examples
RUN cargo build --locked --release --features acme --bin flashlin --example benchmark
FROM debian:bookworm-slim
COPY --from=builder /src/target/release/flashlin /usr/local/bin/flashlin
COPY --from=builder /src/target/release/examples/benchmark /usr/local/bin/benchmark
WORKDIR /bench
ENTRYPOINT []
