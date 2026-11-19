FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg ca-certificates tini \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /build/target/release/rust-video-converter-bot /usr/local/bin/converter-bot
RUN useradd --system --uid 10001 --create-home bot && mkdir -p /data && chown -R bot:bot /data
USER bot
ENV WORK_DIR=/data
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/converter-bot"]
