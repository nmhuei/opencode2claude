FROM rust:1.86-alpine AS builder

RUN apk add --no-cache musl-dev gcc

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo build --release --locked

FROM alpine:3.20

COPY --from=builder /app/target/release/opencode2claude /usr/local/bin/opencode2claude

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD wget --no-verbose --tries=1 --spider http://localhost:4000/health || exit 1

EXPOSE 4000

CMD ["opencode2claude"]
