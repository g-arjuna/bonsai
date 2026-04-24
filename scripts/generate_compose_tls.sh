#!/usr/bin/env bash
# Generate a self-signed CA and mTLS certificates for the bonsai compose stack.
# Run once before the first `docker compose --profile distributed up`.
# Certs land in docker/tls/ which is gitignored.
#
# Requirements: openssl >= 1.1 (or LibreSSL)
set -euo pipefail

TLS_DIR="$(dirname "$0")/../docker/tls"
mkdir -p "$TLS_DIR"

# Abort if certs already exist unless --force is passed.
if [ -f "$TLS_DIR/ca.pem" ] && [ "${1:-}" != "--force" ]; then
    echo "TLS certs already exist in $TLS_DIR. Pass --force to regenerate."
    exit 0
fi

echo "Generating bonsai compose mTLS certificates in $TLS_DIR ..."

# CA
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 \
    -days 3650 -nodes \
    -keyout "$TLS_DIR/ca-key.pem" \
    -out    "$TLS_DIR/ca.pem" \
    -subj   "/CN=bonsai-compose-ca"

# Core server cert
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$TLS_DIR/core-key.pem" \
    -out    "$TLS_DIR/core-csr.pem" \
    -subj   "/CN=bonsai-core"

openssl x509 -req \
    -in     "$TLS_DIR/core-csr.pem" \
    -CA     "$TLS_DIR/ca.pem" \
    -CAkey  "$TLS_DIR/ca-key.pem" \
    -CAcreateserial \
    -days   3650 \
    -extfile <(printf "subjectAltName=DNS:bonsai-core,DNS:localhost") \
    -out    "$TLS_DIR/core-cert.pem"

# Collector client cert (shared by all collectors)
openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
    -keyout "$TLS_DIR/collector-key.pem" \
    -out    "$TLS_DIR/collector-csr.pem" \
    -subj   "/CN=bonsai-collector"

openssl x509 -req \
    -in     "$TLS_DIR/collector-csr.pem" \
    -CA     "$TLS_DIR/ca.pem" \
    -CAkey  "$TLS_DIR/ca-key.pem" \
    -CAcreateserial \
    -days   3650 \
    -extfile <(printf "subjectAltName=DNS:bonsai-collector") \
    -out    "$TLS_DIR/collector-cert.pem"

# Clean up CSRs
rm -f "$TLS_DIR"/*.pem.srl "$TLS_DIR"/*-csr.pem

chmod 600 "$TLS_DIR"/*-key.pem
echo "Done. Certs written to $TLS_DIR"
echo ""
echo "Next steps:"
echo "  1. Set BONSAI_VAULT_PASSPHRASE (see .env.example)"
echo "  2. docker compose --profile two-collector up"
echo "  3. scripts/seed_lab_creds.sh (first time only)"
