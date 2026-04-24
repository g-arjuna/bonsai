#!/usr/bin/env bash
# Generate a self-signed CA and mTLS certificates for the bonsai compose stack.
# Run once before the first `docker compose --profile distributed up`.
# Certs land in docker/tls/ which is gitignored.
#
# Each collector gets its own certificate (CN = bonsai-<collector-id>).
# Revoking one collector is a matter of removing its cert from the CA trust;
# other collectors are not affected.
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

# Per-collector client certs.
# Add additional collector IDs to COLLECTOR_IDS to generate more certs.
COLLECTOR_IDS=("collector-1" "collector-2")

for collector_id in "${COLLECTOR_IDS[@]}"; do
    echo "  Generating cert for $collector_id ..."

    openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
        -keyout "$TLS_DIR/${collector_id}-key.pem" \
        -out    "$TLS_DIR/${collector_id}-csr.pem" \
        -subj   "/CN=bonsai-${collector_id}"

    openssl x509 -req \
        -in     "$TLS_DIR/${collector_id}-csr.pem" \
        -CA     "$TLS_DIR/ca.pem" \
        -CAkey  "$TLS_DIR/ca-key.pem" \
        -CAcreateserial \
        -days   3650 \
        -extfile <(printf "subjectAltName=DNS:bonsai-%s" "$collector_id") \
        -out    "$TLS_DIR/${collector_id}-cert.pem"
done

# Clean up CSRs and serial files
rm -f "$TLS_DIR"/*.pem.srl "$TLS_DIR"/*-csr.pem

# 644 so the container user (uid 10001, not the host owner) can read the keys.
# These are lab-only keys — never use 600 here as that breaks volume mounts.
chmod 644 "$TLS_DIR"/*-key.pem
echo "Done. Certs written to $TLS_DIR"
echo ""
echo "Per-collector certs:"
for collector_id in "${COLLECTOR_IDS[@]}"; do
    echo "  ${collector_id}-cert.pem / ${collector_id}-key.pem  (CN=bonsai-${collector_id})"
done
echo ""
echo "To revoke a collector: remove its cert from the CA trust bundle on the core."
echo ""
echo "Next steps:"
echo "  1. Set BONSAI_VAULT_PASSPHRASE (see .env.example)"
echo "  2. docker compose --profile two-collector up"
echo "  3. scripts/seed_lab_creds.sh (first time only)"
