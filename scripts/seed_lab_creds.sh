#!/usr/bin/env bash
# Seed lab device credentials into the bonsai vault for the compose stack.
# Run once after first startup when the vault is empty.
# The aliases set here must match credential_alias values in docker/configs/core.toml.
#
# Usage:
#   scripts/seed_lab_creds.sh
#
# The script prompts interactively so credentials never appear in shell history.
# Works whether you have a local build or only docker compose.
set -euo pipefail

if [ -z "${BONSAI_VAULT_PASSPHRASE:-}" ]; then
    echo "ERROR: BONSAI_VAULT_PASSPHRASE is not set. Source your .env file first."
    exit 1
fi

# Resolve how to run the bonsai CLI.
LOCAL_BINARY="./target/release/bonsai"
if [ -x "$LOCAL_BINARY" ]; then
    run_bonsai() { "$LOCAL_BINARY" "$@"; }
    echo "Using local binary: $LOCAL_BINARY"
else
    # Fall back to running inside the already-built container image.
    # Requires that docker compose has been pulled/built at least once.
    run_bonsai() {
        docker compose run --rm \
            -e BONSAI_VAULT_PASSPHRASE \
            bonsai-core "$@"
    }
    echo "Local binary not found — using container image via docker compose run."
    echo "(Run 'cargo build --release' if you prefer the local path.)"
fi

echo ""
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
    run_bonsai credentials add --alias "$alias" --username "$username" --password "$password"
    echo "  -> Added alias '$alias'"
}

prompt_cred "lab-admin"  "Nokia SR Linux devices (admin)"
prompt_cred "lab-cisco"  "Cisco XRd devices (cisco)"

echo ""
echo "Credentials seeded. Run 'docker compose --profile two-collector restart bonsai-core' to apply."
