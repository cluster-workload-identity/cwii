# syntax=docker/dockerfile:1
#
# Multi-stage build for the cwii webhook. The final image is distroless (no shell, no package
# manager) and runs as the non-root uid 65532. Built for linux/amd64 and linux/arm64 via
# `docker buildx --platform linux/amd64,linux/arm64`; under buildx each target architecture is
# built in its own (emulated) stage, so the produced binary always matches the image platform.

# Base images are pinned by digest for reproducibility (tag kept for readability; Dependabot's
# docker ecosystem bumps both the tag and the digest).
# cargo-chef base: lets us cache the dependency build separately from the source build.
FROM rust:1.96.0-slim-trixie@sha256:082a5849a6870672b5f7a5bf4eddc71723fce38756fd834a0d734a5306a310ab AS chef
RUN cargo install cargo-chef --locked
WORKDIR /src

# Compute the dependency recipe (cheap, source-independent).
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Build dependencies (cached) then the workspace binary.
FROM chef AS builder
# cmake + clang are needed to build aws-lc-sys (the aws-lc-rs rustls crypto backend).
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config cmake clang libclang-dev \
 && rm -rf /var/lib/apt/lists/*
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
# --locked enforces the committed Cargo.lock: a stale lockfile fails the build instead of drifting.
RUN cargo build --release --locked --bin cwii \
 && cp target/release/cwii /cwii

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:b0ae8e989418b458e0f25489bc3be523718938a2b70864cc0f6a00af1ddbd985
COPY --from=builder /cwii /usr/local/bin/cwii
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/cwii"]
