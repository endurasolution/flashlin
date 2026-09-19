# syntax=docker/dockerfile:1
ARG RUST_IMAGE=rust:1.95-bookworm
FROM ${RUST_IMAGE} AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples ./examples
ARG FEATURES=acme
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release --features "${FEATURES}" --bin flashlin --example benchmark \
    && mkdir -p /out \
    && cp target/release/flashlin /out/flashlin \
    && cp target/release/examples/benchmark /out/benchmark \
    && rustc --version > /out/build.txt \
    && cargo --version >> /out/build.txt \
    && printf 'features=%s\n' "${FEATURES}" >> /out/build.txt \
    && find src examples -type f -print0 | sort -z | xargs -0 sha256sum > /out/source-sha256.txt \
    && sha256sum Cargo.toml Cargo.lock >> /out/source-sha256.txt

FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends nginx apache2 python3 curl ca-certificates util-linux procps iproute2 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /out/ /opt/bin/
COPY benchmarks/ubuntu/ /opt/bench/
# Generate fixtures inside the Linux image. Serving a Windows bind mount would
# mostly measure the host/container filesystem bridge instead of these servers.
RUN python3 /opt/bench/fixtures.py /srv/bench \
    && mkdir -p /results /run/apache2 \
    && chmod -R a+rX /srv/bench /opt/bench /opt/bin
WORKDIR /opt/bench
ENTRYPOINT ["python3", "/opt/bench/run.py"]
