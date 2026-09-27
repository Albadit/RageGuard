# syntax=docker/dockerfile:1
#
# Optional container image for the RageGuard bot. During development the bot normally runs on the
# host with `cargo run`; this image is for hosts where that is inconvenient (e.g. Windows with
# Smart App Control enabled, which blocks freshly compiled binaries).
#
#   docker compose --profile bot up -d --build

# Toolchain with the native dependencies songbird needs (Opus via cmake/pkg-config).
# Also usable for running the test suite in Linux:
#   docker build --target toolchain -t rageguard-dev bot/
FROM rust:1-bookworm AS toolchain
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config libopus-dev \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add clippy rustfmt
WORKDIR /app

FROM toolchain AS builder
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked \
    && cp target/release/rageguard /usr/local/bin/rageguard

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libopus0 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home rageguard
COPY --from=builder /usr/local/bin/rageguard /usr/local/bin/rageguard
COPY rageguard.toml /app/rageguard.toml
# Saved per-server settings (the log channel chosen in Discord); mounted as a volume.
RUN mkdir -p /app/data && chown rageguard /app/data
WORKDIR /app
USER rageguard
ENTRYPOINT ["rageguard"]
