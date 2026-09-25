# flinch — one image, two binaries, a React UI baked at build time.
# Node exists only in the builder stage; the runtime is Rust + static assets.
# Base images are pinned by digest; Dependabot moves tag and digest together.
FROM node:25-alpine@sha256:bdf2cca6fe3dabd014ea60163eca3f0f7015fbd5c7ee1b0e9ccb4ced6eb02ef4 AS ui
WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY frontend/ .
RUN npm run build

FROM rust:1.98-alpine@sha256:7cc1c22d77d9432f7fe012a70e6d3e555af54c2a6832700ed7d553f1769ae89f AS builder
WORKDIR /build
RUN apk add --no-cache musl-dev
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release --locked -p flinch-archive --bin flinch-arrd --bin flinch-fit \
    && cargo build --release --locked -p flinch-web --bin flinch-web --bin flinch-demo

FROM alpine:3.24@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6
RUN apk add --no-cache ca-certificates tzdata && adduser -D -u 1000 flinch
WORKDIR /data
COPY --from=builder /build/target/release/flinch-arrd /usr/local/bin/flinch-arrd
COPY --from=builder /build/target/release/flinch-fit /usr/local/bin/flinch-fit
COPY --from=builder /build/target/release/flinch-web /usr/local/bin/flinch-web
COPY --from=builder /build/target/release/flinch-demo /usr/local/bin/flinch-demo
COPY --from=ui /build/frontend/dist /srv/web
ENV FLINCH_WEB_DIR=/srv/web
EXPOSE 7911
# Numeric, so Kubernetes can verify runAsNonRoot without the manifests' help.
USER 1000:1000
CMD ["/usr/local/bin/flinch-web"]
