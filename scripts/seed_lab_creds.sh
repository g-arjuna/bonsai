#!/usr/bin/env bash
# Seed lab device credentials into the bonsai vault for the compose stack.
# Run once after first startup when the vault is empty.
# The aliases set here must match credential_alias values in docker/configs/core.toml.
#
# Usage:
#   scripts/seed_lab_creds.sh
#
# The script prompts interactively so credentials never appear in shell history.
set -euo pipefail

BINARY="./target/release/bonsai"
if [ ! -x "$BINARY" ]; then
    echo "ERROR: $BINARY not found. Run 'cargo build --release' first."
    exit 1
fi

if [ -z "${BONSAI_VAULT_PASSPHRASE:-}" ]; then
    echo "ERROR: BONSAI_VAULT_PASSPHRASE is not set. Source your .env file first."
    exit 1
fi

echo "Seeding lab credentials into bonsai vault ..."
echo "These are stored encrypted in the vault and never written to config files."
echo ""

prompt_cred() {
    local alias="$1"
    local hint="$2"
    local username password
    read -rp "Username for '$alias' ($hint): " username
    read -rsp "Password for '$alias': " password
    echo ""
    "$BINARY" credentials add --alias "$alias" --username "$username" --password "$password"
    echo "  -> Added alias '$alias'"
}

prompt_cred "lab-admin"  "Nokia SR Linux devices (admin)"
prompt_cred "lab-cisco"  "Cisco XRd devices (cisco)"

echo ""
echo "Credentials seeded. Run 'docker compose --profile two-collector restart bonsai-core' to apply."
