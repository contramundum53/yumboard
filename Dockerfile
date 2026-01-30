FROM rust:1.93 AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY client/Cargo.toml client/Cargo.toml
COPY server/Cargo.toml server/Cargo.toml
COPY shared/Cargo.toml shared/Cargo.toml
COPY client/src client/src
COPY server/src server/src
COPY shared/src shared/src
COPY public public

RUN cargo install wasm-pack
RUN wasm-pack build --release client --target web --out-dir ../public/pkg
RUN cargo build --release -p yumboard_server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/yumboard_server /app/yumboard_server
COPY --from=builder /app/public /app/public

ENV PORT=3000
EXPOSE 3000

CMD ["/app/yumboard_server"]
