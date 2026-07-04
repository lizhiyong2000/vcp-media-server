# syntax=docker/dockerfile:1

FROM rust:1.83-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release --bin vcp-media-server

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        ffmpeg \
        libcap2-bin \
        wget \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -u 10001 -g nogroup vcp

WORKDIR /app

COPY --from=builder /app/target/release/vcp-media-server /usr/local/bin/vcp-media-server
COPY deploy/docker/config.toml /etc/vcp-media-server/config.toml

RUN setcap 'cap_net_bind_service=+ep' /usr/local/bin/vcp-media-server \
    && mkdir -p /data/saving /data/logs \
    && chown -R vcp:nogroup /data /etc/vcp-media-server

USER vcp

ENV VCP_CONFIG_PATH=/etc/vcp-media-server/config.toml

EXPOSE 1935 8554 8081 9080

VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD wget -qO- http://127.0.0.1:8081/health >/dev/null || exit 1

ENTRYPOINT ["/usr/local/bin/vcp-media-server"]
CMD ["/etc/vcp-media-server/config.toml"]
