#!/bin/bash
set -e

# Install sccache for Rust compilation caching.

if command -v sccache >/dev/null 2>&1; then
    echo "sccache is already installed."
    sccache --version
    exit 0
fi

echo "Installing sccache..."
cargo install sccache

echo "sccache installed successfully."
sccache --version
