# syntax=docker/dockerfile:1

# ============================================================
# Stage: chef — 安装基础工具和系统库
# ============================================================
FROM rust:1-bookworm AS chef

# 改用阿里云镜像源加速 APT 下载
RUN sed -i 's@//deb.debian.org/@//mirrors.aliyun.com/@g' /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libclang-dev \
        libavutil-dev \
        libavformat-dev \
        libavcodec-dev \
        libavdevice-dev \
        libavfilter-dev \
        libswscale-dev \
        libswresample-dev \
        mold \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install cargo-chef --locked

WORKDIR /app

# ============================================================
# Stage: planner — 分析依赖、生成 recipe
# ============================================================
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

# ============================================================
# Stage: builder — 先编译依赖（缓存层）、再编译源码
# ============================================================
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json

# 编译依赖（Cargo.toml 不变时完全缓存，跳过 ffmpeg-sys-next / webrtc 等大仓）
RUN cargo chef cook --release --recipe-path recipe.json

# 复制源码
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# 用 mold 链接器 + LTO=off 加速编译
ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold -C lto=off"

RUN cargo build --release --bin vcp-media-server

# ============================================================
# Stage: runtime — 最小运行镜像
# ============================================================
FROM debian:bookworm-slim AS runtime

RUN sed -i 's@//deb.debian.org/@//mirrors.aliyun.com/@g' /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
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
