FROM rust:1.93-bookworm AS builder

WORKDIR /app

COPY . .

RUN cargo build --release --locked -p gateway-http -p tenant-api -p control-plane

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

ARG APP_BIN
ENV APP_BIN=${APP_BIN}

WORKDIR /app

COPY --from=builder /app/target/release/gateway-http /usr/local/bin/gateway-http
COPY --from=builder /app/target/release/tenant-api /usr/local/bin/tenant-api
COPY --from=builder /app/target/release/control-plane /usr/local/bin/control-plane

CMD ["/bin/sh", "-lc", "exec /usr/local/bin/${APP_BIN}"]

