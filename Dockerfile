# syntax=docker/dockerfile:1
#
# Multi-stage build for the cwii webhook. The final image is distroless (no shell, no package
# manager) and runs as the non-root uid 65532. Built for linux/amd64 and linux/arm64 via
# `docker buildx --platform linux/amd64,linux/arm64`; under buildx each target architecture is
# built in its own (emulated) stage, so the produced binary always matches the image platform.

# Base images are pinned by digest for reproducibility (tag kept for readability; Dependabot's
# docker ecosystem bumps both the tag and the digest).
# cargo-chef base: lets us cache the dependency build separately from the source build.
FROM rust:1.96.0-slim-trixie@sha256:3b05f7c617a200c41c3506097f0d15fc193a1c93bfd8f141007b47cac8f95d3c AS chef
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

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:d3cda6e91129130d7229a1806b6a73d292ef245ab032da7851907798024cefba
COPY --from=builder /cwii /usr/local/bin/cwii
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/cwii"]
