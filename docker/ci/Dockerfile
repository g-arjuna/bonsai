# syntax=docker/dockerfile:1.7
#
# Bonsai local CI image (development-only, BN-019-B6 / secure-core WP1).
#
# Purpose: reproduce the canonical `scripts/ci/gates.sh` environment
# locally while GitHub-hosted runners are unavailable. This image is
# SELF-CONTAINED for the toolchain: the pinned Rust toolchain
# (rust-toolchain.toml channel), rustfmt and sccache are installed into
# the image at build time. At run time NO developer home directory,
# ~/.cargo, ~/.rustup, registry credentials or rustup config is mounted.
# Only dedicated CI cache directories (cargo registry, sccache,
# Playwright browsers, npm) are bind-mounted — see
# scripts/ci/run-local.sh.
#
# Isolation contract (see docs/DEVELOPMENT.md "Local containerized CI"):
#   - non-root user, no docker socket, no privileged mode
#   - no production databases, no protected soak evidence
#   - trusted mode may reuse dedicated warm caches and (explicitly)
#     the shared Cargo target; untrusted mode gets a fresh per-run
#     cache root and never shares executable caches or target
#
# Resource policy (8 CPU / ~46 GB host): run with
#   --memory=24g --cpus=4 --memory-swap=24g \
#   -e CARGO_BUILD_JOBS=2
# and keep >= 8-10 GB free on the host for the desktop session.

FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    CI=true \
    CARGO_TERM_COLOR=always

# OS-level prerequisites: native build tools, Python 3.12 + venv, git,
# Node 20 (UI build + Playwright driver), Playwright chromium runtime
# libraries. Must match the GitHub workflow's ubuntu-latest + Node 20.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        clang \
        cmake \
        g++ \
        gcc \
        git \
        make \
        mold \
        pkg-config \
        protobuf-compiler \
        python3.12 \
        python3.12-venv \
        python3-pip \
        curl \
        unzip \
        xz-utils \
        libssl-dev \
        libclang-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

RUN apt-get update && apt-get install -y --no-install-recommends \
        libnss3 libnspr4 libatk1.0-0t64 libatk-bridge2.0-0t64 \
        libcups2t64 libdrm2 libxkbcommon0 libxcomposite1 libxdamage1 \
        libxfixes3 libxrandr2 libgbm1 libasound2t64 libpango-1.0-0 \
        libcairo2 libatspi2.0-0t64 \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 1500 --shell /bin/bash bonsai-ci

# ── Pinned Rust toolchain, installed IN the image ──────────────────────
# Channel must match rust-toolchain.toml (1.95.0). rustup is used only
# at build time; at run time the toolchain bin directory is put on PATH
# directly, so no rustup proxies, RUSTUP_HOME writes or developer
# toolchain state are needed inside the container.
ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain none --profile minimal \
    && rustup toolchain install 1.95.0 --profile minimal --component rustfmt \
    && rustup default 1.95.0 \
    && rustc --version && cargo --version && rustfmt --version

# ── sccache (mandatory rustc-wrapper per .cargo/config.toml) ───────────
# Static musl binary; version pinned to the host-verified 0.15.0.
ARG SCCACHE_VERSION=0.15.0
RUN curl -fsSL \
        "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
    | tar -xz --strip-components=1 -C /usr/local/bin \
        "sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" \
    && sccache --version

# Ephemeral container HOME (no host home is ever mounted).
RUN mkdir -p /ci/home /ci-cache && chown -R bonsai-ci:bonsai-ci /ci/home /ci-cache

WORKDIR /ci
USER bonsai-ci
CMD ["bash"]
