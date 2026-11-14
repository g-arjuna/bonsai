# syntax=docker/dockerfile:1.7
#
# Bonsai local CI image (development-only, BN-019-B6 / secure-core WP1).
#
# Purpose: reproduce the canonical `scripts/ci/gates.sh` environment
# locally while GitHub-hosted runners are unavailable. This image
# contains OS-level prerequisites only — the Rust toolchain, cargo
# registry, sccache cache and Cargo target are bind-mounted from the
# host at run time (see scripts/ci/run-local.sh) so fingerprints and
# compiler caches stay compatible and no second ~140 GB target tree is
# created. The image itself holds NO repository content, credentials,
# databases or runtime state.
#
# Resource policy (8 CPU / ~46 GB host): run with
#   --memory=24g --cpus=4 --memory-swap=24g \
#   -e CARGO_BUILD_JOBS=2
# and keep >= 8-10 GB free on the host for the desktop session.

FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive \
    CI=true \
    CARGO_TERM_COLOR=always

# Pinned toolchain components (match rust-toolchain.toml channel 1.95.0
# and the GitHub workflow's ubuntu-latest expectations).
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

# Node 20 (UI build + Playwright driver). Pinned to the workflow's major.
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# Non-root CI user. The host uid is passed at runtime via --user so
# bind-mounted caches/target stay owned by the developer identity.
RUN useradd --create-home --uid 1500 --shell /bin/bash bonsai-ci

# Playwright Chromium runtime libraries (browser binaries themselves are
# bind-mounted from the host cache — see run-local.sh).
RUN apt-get update && apt-get install -y --no-install-recommends \
        libnss3 libnspr4 libatk1.0-0t64 libatk-bridge2.0-0t64 \
        libcups2t64 libdrm2 libxkbcommon0 libxcomposite1 libxdamage1 \
        libxfixes3 libxrandr2 libgbm1 libasound2t64 libpango-1.0-0 \
        libcairo2 libatspi2.0-0t64 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /ci
USER bonsai-ci
CMD ["bash"]
