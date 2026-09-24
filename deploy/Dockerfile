# flinch — one image, two binaries, a React UI baked at build time.
# Node exists only in the builder stage; the runtime is Rust + static assets.
FROM node:22-alpine AS ui
WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json* ./
RUN npm install --no-audit --no-fund || (npm install --no-audit --no-fund --legacy-peer-deps)
COPY frontend/ .
RUN npm run build

FROM rust:1.98-alpine AS builder
WORKDIR /build
RUN apk add --no-cache musl-dev
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY frontend ./frontend
RUN cargo build --release -p flinch-archive --bin flinch-arrd --bin flinch-fit \
    && cargo build --release -p flinch-web --bin flinch-web --bin flinch-demo

FROM alpine:3.20
RUN apk add --no-cache ca-certificates tzdata && adduser -D -u 1000 flinch
WORKDIR /data
COPY --from=builder /build/target/release/flinch-arrd /usr/local/bin/flinch-arrd
COPY --from=builder /build/target/release/flinch-fit /usr/local/bin/flinch-fit
COPY --from=builder /build/target/release/flinch-web /usr/local/bin/flinch-web
COPY --from=builder /build/target/release/flinch-demo /usr/local/bin/flinch-demo
COPY --from=ui /build/frontend/dist /srv/web
ENV FLINCH_WEB_DIR=/srv/web
EXPOSE 7911
USER flinch
CMD ["/usr/local/bin/flinch-web"]
