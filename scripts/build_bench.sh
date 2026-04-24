#!/bin/bash
set -e

# Establish build performance baseline for Bonsai v5.
# Captures Rust Clean, Incremental, UI, and Docker build times.

RESULTS_FILE="docs/build_performance.md"
DATE=$(date -u +"%Y-%m-%d %H:%M:%S UTC")
HOSTNAME=$(hostname)

echo "---" >> $RESULTS_FILE
echo "### Build Benchmark: $DATE ($HOSTNAME)" >> $RESULTS_FILE
echo "" >> $RESULTS_FILE
echo "| Task | Duration | Notes |" >> $RESULTS_FILE
echo "|---|---|---|" >> $RESULTS_FILE

function bench() {
    local label=$1
    local cmd=$2
    local notes=$3
    
    echo "Benchmarking: $label..."
    start=$(date +%s)
    eval "$cmd"
    end=$(date +%s)
    duration=$((end - start))
    
    # Format duration as M:SS
    min=$((duration / 60))
    sec=$((duration % 60))
    printf -v duration_fmt "%d:%02d" $min $sec
    
    echo "| $label | $duration_fmt | $notes |" >> $RESULTS_FILE
}

# 1. Clean Rust build
bench "cargo build --release (clean)" "cargo clean && cargo build --release" "Cold cache, all dependencies"

# 2. Incremental Rust build (touch one file)
touch src/main.rs
bench "cargo build --release (incremental)" "cargo build --release" "Source change in src/main.rs"

# 3. Rust check
bench "cargo check --release" "cargo check --release" ""

# 4. Rust test
bench "cargo test --release" "cargo test --release" ""

# 5. UI build
bench "npm run build (UI)" "cd ui && npm ci && npm run build && cd .." "Node.js v20.x"

# 6. Docker build (cold)
bench "docker build (clean)" "docker build --no-cache -f docker/Dockerfile.bonsai -t bonsai:bench ." ""

# 7. Docker build (BuildKit cache hit)
touch src/lib.rs
bench "docker build (cache hit)" "docker build -f docker/Dockerfile.bonsai -t bonsai:bench ." "BuildKit cache hit on dependencies"

# 8. Sizes
BIN_SIZE=$(du -h target/release/bonsai | cut -f1)
IMG_SIZE=$(docker images bonsai:bench --format "{{.Size}}")

echo "" >> $RESULTS_FILE
echo "**Release Binary Size**: $BIN_SIZE" >> $RESULTS_FILE
echo "**Docker Image Size**: $IMG_SIZE" >> $RESULTS_FILE
echo "" >> $RESULTS_FILE

echo "Benchmark complete. Results appended to $RESULTS_FILE"
