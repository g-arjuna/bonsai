#!/bin/bash
set -e

# Audit Rust dependencies for duplicates, outdated versions, and security advisories.

RESULTS_FILE="docs/build_performance.md"
DATE=$(date -u +"%Y-%m-%d %H:%M:%S UTC")

echo "---" >> $RESULTS_FILE
echo "### Dependency Audit: $DATE" >> $RESULTS_FILE
echo "" >> $RESULTS_FILE

echo "Checking for duplicate dependencies..."
echo "#### Duplicate Dependencies" >> $RESULTS_FILE
echo "\`\`\`" >> $RESULTS_FILE
cargo tree --duplicates >> $RESULTS_FILE || true
echo "\`\`\`" >> $RESULTS_FILE
echo "" >> $RESULTS_FILE

if command -v cargo-audit >/dev/null 2>&1; then
    echo "Checking for security advisories..."
    echo "#### Security Advisories" >> $RESULTS_FILE
    echo "\`\`\`" >> $RESULTS_FILE
    cargo audit >> $RESULTS_FILE || true
    echo "\`\`\`" >> $RESULTS_FILE
    echo "" >> $RESULTS_FILE
else
    echo "cargo-audit not installed, skipping security check."
fi

if command -v cargo-outdated >/dev/null 2>&1; then
    echo "Checking for outdated dependencies..."
    echo "#### Outdated Dependencies" >> $RESULTS_FILE
    echo "\`\`\`" >> $RESULTS_FILE
    cargo outdated >> $RESULTS_FILE || true
    echo "\`\`\`" >> $RESULTS_FILE
    echo "" >> $RESULTS_FILE
else
    echo "cargo-outdated not installed, skipping outdated check."
fi

echo "Dependency audit complete. Results appended to $RESULTS_FILE"
