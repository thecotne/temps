# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

# Toolchain image for scripts/fork/build-image.sh.
#
# The release binary must run on the runtime image (debian:bookworm-slim, glibc
# 2.36). release.yml and fork-image.yml therefore compile on ubuntu-22.04 (glibc
# 2.35); a binary linked on a newer host (for example Debian 13, glibc 2.41)
# fails at start-up with "GLIBC_2.38 not found". This image is that runner's
# equivalent: same apt packages as the "Install build dependencies" step plus a
# rustup toolchain.
#
# Built without a context: docker build --tag <name> - < scripts/fork/Dockerfile.builder

FROM ubuntu:22.04

ARG RUST_TOOLCHAIN=stable

ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    cmake \
    curl \
    file \
    git \
    libssl-dev \
    pkg-config \
    protobuf-compiler && \
    apt-get clean

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" && \
    chmod -R a+rX /usr/local/rustup /usr/local/cargo && \
    rustc --version && cargo --version && ldd --version | head -n 1
