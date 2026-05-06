FROM rust:1.84.1 as builder

WORKDIR /app

COPY . .
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/marinade-bond-monitor /usr/local/bin/marinade-bond-monitor

CMD ["marinade-bond-monitor"]
