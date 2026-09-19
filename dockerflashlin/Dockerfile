# ─── Stage 1: Build FlashLin from source ────────────────────────────────────
# Build context must be the flashlin project root:
#   docker build -f dockerflashlin/Dockerfile -t flashlin .
FROM rust:1.95-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --locked --release --features acme,http3 --bin flashlin

# ─── Stage 2: Minimal runtime image ─────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -u 1001 -g 0 -s /bin/false -d /var/www flashlin \
    && mkdir -p /var/www/html /etc/flashlin /var/log/flashlin /var/lib/flashlin/acme \
    && chown -R 1001:0 /var/www/html /var/log/flashlin /var/lib/flashlin \
    && chmod 700 /var/lib/flashlin/acme

COPY --from=builder /src/target/release/flashlin /usr/local/bin/flashlin

COPY dockerflashlin/config/server.toml /etc/flashlin/server.toml
COPY dockerflashlin/public/             /var/www/html/
COPY dockerflashlin/entrypoint.sh       /entrypoint.sh

RUN chmod +x /entrypoint.sh

EXPOSE 80 443
EXPOSE 443/udp

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -sf http://localhost:80/ > /dev/null || exit 1

USER 1001

ENTRYPOINT ["/entrypoint.sh"]
