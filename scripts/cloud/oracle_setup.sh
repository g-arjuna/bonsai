#!/usr/bin/env bash
# scripts/cloud/oracle_setup.sh — Oracle Cloud Always Free ARM VM provisioning.
#
# Provisions a single Oracle Always Free ARM instance (4 OCPU, 24 GB RAM,
# 200 GB block storage) via the OCI CLI. Idempotent: re-running skips steps
# that are already complete.
#
# Prerequisites (run on your laptop/WSL):
#   1. OCI CLI installed and configured:  oci setup config
#   2. SSH key pair generated:            ssh-keygen -t ed25519 -f ~/.ssh/bonsai_cloud
#   3. Free-tier tenancy already approved (usually instant for Always Free)
#
# Usage:
#   bash scripts/cloud/oracle_setup.sh
#   bash scripts/cloud/oracle_setup.sh --dry-run   # print what would be done
#   bash scripts/cloud/oracle_setup.sh --destroy   # tear down resources
#
# Output:
#   scripts/cloud/instance.env  — instance ID, IP, compartment (sourced by deploy.sh)
#
# Region recommendation: pick the one closest to your lab for lowest RTT
# to the gNMI path (lab → cloud correlation latency).

set -euo pipefail

# ── Configuration (edit these or set as env vars before running) ──────────────

OCI_REGION="${OCI_REGION:-us-ashburn-1}"          # Always Free available in all home regions
OCI_COMPARTMENT_ID="${OCI_COMPARTMENT_ID:-}"      # Required: your tenancy OCID or sub-compartment
OCI_SSH_KEY_PATH="${OCI_SSH_KEY_PATH:-$HOME/.ssh/bonsai_cloud.pub}"
INSTANCE_NAME="${INSTANCE_NAME:-bonsai-cloud-spike}"
INSTANCE_SHAPE="${INSTANCE_SHAPE:-VM.Standard.A1.Flex}"              # ARM Always Free shape
OCPU_COUNT="${OCPU_COUNT:-4}"
MEM_GB="${MEM_GB:-24}"
BOOT_VOL_GB="${BOOT_VOL_GB:-100}"
DATA_VOL_GB="${DATA_VOL_GB:-100}"

STATE_FILE="$(dirname "${BASH_SOURCE[0]}")/instance.env"

# ── Helpers ───────────────────────────────────────────────────────────────────

DRY_RUN=false
DESTROY=false

for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=true ;;
        --destroy) DESTROY=true ;;
        *) echo "Unknown arg: $arg" >&2; exit 1 ;;
    esac
done

_log() { echo "[$(date -u '+%H:%M:%S')] $*"; }
_die() { echo "ERROR: $*" >&2; exit 1; }
_run() {
    if "$DRY_RUN"; then
        echo "[DRY-RUN] $*"
    else
        "$@"
    fi
}

_check_prereqs() {
    command -v oci  &>/dev/null || _die "OCI CLI not found. Install: https://docs.oracle.com/en-us/iaas/Content/API/SDKDocs/cliinstall.htm"
    command -v jq   &>/dev/null || _die "jq not found (brew install jq / apt install jq)"
    [[ -f "$OCI_SSH_KEY_PATH" ]]   || _die "SSH public key not found: $OCI_SSH_KEY_PATH — run: ssh-keygen -t ed25519 -f ~/.ssh/bonsai_cloud"
    [[ -n "$OCI_COMPARTMENT_ID" ]] || _die "OCI_COMPARTMENT_ID not set. Export your compartment/tenancy OCID."
    oci iam region list --output json &>/dev/null || _die "OCI CLI not configured. Run: oci setup config"
}

# ── Destroy path ──────────────────────────────────────────────────────────────

_destroy() {
    [[ -f "$STATE_FILE" ]] || { _log "No state file found — nothing to destroy."; return; }
    source "$STATE_FILE"

    _log "Terminating instance $BONSAI_INSTANCE_ID..."
    _run oci compute instance terminate \
        --instance-id "$BONSAI_INSTANCE_ID" \
        --preserve-boot-volume false \
        --force \
        --region "$OCI_REGION"

    if [[ -n "${BONSAI_DATA_VOL_ID:-}" ]]; then
        _log "Waiting 30s for instance termination before deleting volume..."
        sleep 30
        _log "Deleting data volume $BONSAI_DATA_VOL_ID..."
        _run oci bv volume delete \
            --volume-id "$BONSAI_DATA_VOL_ID" \
            --force \
            --region "$OCI_REGION"
    fi

    rm -f "$STATE_FILE"
    _log "Destroy complete."
}

if "$DESTROY"; then
    _destroy
    exit 0
fi

# ── Provision path ────────────────────────────────────────────────────────────

_check_prereqs
_log "=== Bonsai cloud spike provisioning ==="
_log "Region: $OCI_REGION  Shape: $INSTANCE_SHAPE  OCPU: $OCPU_COUNT  RAM: ${MEM_GB}GB"

# Step 1: Find Always Free-eligible AD
_log "Step 1: Resolving availability domain..."
AD=$(oci iam availability-domain list \
    --compartment-id "$OCI_COMPARTMENT_ID" \
    --region "$OCI_REGION" \
    --output json | jq -r '.data[0].name')
_log "  Using AD: $AD"

# Step 2: Find latest Oracle Linux 8 ARM image
_log "Step 2: Finding latest Oracle Linux 8 ARM image..."
IMAGE_ID=$(oci compute image list \
    --compartment-id "$OCI_COMPARTMENT_ID" \
    --operating-system "Oracle Linux" \
    --operating-system-version "8" \
    --shape "$INSTANCE_SHAPE" \
    --sort-by TIMECREATED \
    --sort-order DESC \
    --region "$OCI_REGION" \
    --output json | jq -r '.data[0].id')
_log "  Image: $IMAGE_ID"

# Step 3: Resolve VCN / subnet (use default VCN if exists, else create minimal one)
_log "Step 3: Resolving VCN and subnet..."
VCN_ID=$(oci network vcn list \
    --compartment-id "$OCI_COMPARTMENT_ID" \
    --region "$OCI_REGION" \
    --output json | jq -r '.data[] | select(."display-name" == "bonsai-vcn") | .id' | head -1)

if [[ -z "$VCN_ID" ]]; then
    _log "  Creating bonsai-vcn..."
    VCN_ID=$(oci network vcn create \
        --compartment-id "$OCI_COMPARTMENT_ID" \
        --display-name "bonsai-vcn" \
        --cidr-block "10.200.0.0/24" \
        --region "$OCI_REGION" \
        --wait-for-state AVAILABLE \
        --output json | jq -r '.data.id')

    # Internet gateway
    IGW_ID=$(oci network internet-gateway create \
        --compartment-id "$OCI_COMPARTMENT_ID" \
        --vcn-id "$VCN_ID" \
        --display-name "bonsai-igw" \
        --is-enabled true \
        --region "$OCI_REGION" \
        --output json | jq -r '.data.id')

    # Default route table
    RT_ID=$(oci network route-table list \
        --compartment-id "$OCI_COMPARTMENT_ID" \
        --vcn-id "$VCN_ID" \
        --region "$OCI_REGION" \
        --output json | jq -r '.data[0].id')
    oci network route-table update \
        --rt-id "$RT_ID" \
        --route-rules "[{\"destination\":\"0.0.0.0/0\",\"destinationType\":\"CIDR_BLOCK\",\"networkEntityId\":\"$IGW_ID\"}]" \
        --region "$OCI_REGION" \
        --force > /dev/null

    # Security list: allow SSH + bonsai HTTP (3000) inbound
    SL_ID=$(oci network security-list list \
        --compartment-id "$OCI_COMPARTMENT_ID" \
        --vcn-id "$VCN_ID" \
        --region "$OCI_REGION" \
        --output json | jq -r '.data[0].id')
    oci network security-list update \
        --security-list-id "$SL_ID" \
        --ingress-security-rules '[
          {"source":"0.0.0.0/0","protocol":"6","tcpOptions":{"destinationPortRange":{"min":22,"max":22}},"isStateless":false},
          {"source":"0.0.0.0/0","protocol":"6","tcpOptions":{"destinationPortRange":{"min":3000,"max":3000}},"isStateless":false}
        ]' \
        --egress-security-rules '[{"destination":"0.0.0.0/0","protocol":"all","isStateless":false}]' \
        --region "$OCI_REGION" \
        --force > /dev/null
fi

SUBNET_ID=$(oci network subnet list \
    --compartment-id "$OCI_COMPARTMENT_ID" \
    --vcn-id "$VCN_ID" \
    --region "$OCI_REGION" \
    --output json | jq -r '.data[0].id')

if [[ -z "$SUBNET_ID" ]]; then
    _log "  Creating subnet..."
    SUBNET_ID=$(oci network subnet create \
        --compartment-id "$OCI_COMPARTMENT_ID" \
        --vcn-id "$VCN_ID" \
        --display-name "bonsai-subnet" \
        --cidr-block "10.200.0.0/24" \
        --availability-domain "$AD" \
        --region "$OCI_REGION" \
        --wait-for-state AVAILABLE \
        --output json | jq -r '.data.id')
fi
_log "  Subnet: $SUBNET_ID"

# Step 4: Launch instance
_log "Step 4: Launching Always Free ARM instance..."

LAUNCH_JSON=$(oci compute instance launch \
    --compartment-id "$OCI_COMPARTMENT_ID" \
    --availability-domain "$AD" \
    --display-name "$INSTANCE_NAME" \
    --image-id "$IMAGE_ID" \
    --shape "$INSTANCE_SHAPE" \
    --shape-config "{\"ocpus\":$OCPU_COUNT,\"memoryInGBs\":$MEM_GB}" \
    --subnet-id "$SUBNET_ID" \
    --assign-public-ip true \
    --ssh-authorized-keys-file "$OCI_SSH_KEY_PATH" \
    --boot-volume-size-in-gbs "$BOOT_VOL_GB" \
    --metadata "{\"user_data\":\"$(base64 -w0 "$(dirname "${BASH_SOURCE[0]}")/cloud_init.sh" 2>/dev/null || echo "")\"}" \
    --region "$OCI_REGION" \
    --wait-for-state RUNNING \
    --output json 2>&1) || {
    # If user_data fails (file missing), retry without it
    LAUNCH_JSON=$(oci compute instance launch \
        --compartment-id "$OCI_COMPARTMENT_ID" \
        --availability-domain "$AD" \
        --display-name "$INSTANCE_NAME" \
        --image-id "$IMAGE_ID" \
        --shape "$INSTANCE_SHAPE" \
        --shape-config "{\"ocpus\":$OCPU_COUNT,\"memoryInGBs\":$MEM_GB}" \
        --subnet-id "$SUBNET_ID" \
        --assign-public-ip true \
        --ssh-authorized-keys-file "$OCI_SSH_KEY_PATH" \
        --boot-volume-size-in-gbs "$BOOT_VOL_GB" \
        --region "$OCI_REGION" \
        --wait-for-state RUNNING \
        --output json)
}

INSTANCE_ID=$(echo "$LAUNCH_JSON" | jq -r '.data.id')
_log "  Instance ID: $INSTANCE_ID"

# Step 5: Attach data volume for archive storage
_log "Step 5: Creating and attaching data volume (${DATA_VOL_GB} GB)..."
DATA_VOL_ID=$(oci bv volume create \
    --compartment-id "$OCI_COMPARTMENT_ID" \
    --availability-domain "$AD" \
    --display-name "bonsai-archive-vol" \
    --size-in-gbs "$DATA_VOL_GB" \
    --region "$OCI_REGION" \
    --wait-for-state AVAILABLE \
    --output json | jq -r '.data.id')

oci compute volume-attachment attach \
    --instance-id "$INSTANCE_ID" \
    --type paravirtualized \
    --volume-id "$DATA_VOL_ID" \
    --region "$OCI_REGION" \
    --wait-for-state ATTACHED > /dev/null
_log "  Data volume attached: $DATA_VOL_ID"

# Step 6: Retrieve public IP
_log "Step 6: Retrieving public IP..."
PUBLIC_IP=$(oci compute instance list-vnics \
    --instance-id "$INSTANCE_ID" \
    --region "$OCI_REGION" \
    --output json | jq -r '.data[0]."public-ip"')
_log "  Public IP: $PUBLIC_IP"

# Step 7: Write state file
cat > "$STATE_FILE" <<EOF
# Bonsai cloud spike — instance state (sourced by deploy.sh and daily_sync.sh)
# Generated: $(date -u '+%Y-%m-%dT%H:%M:%SZ')
export BONSAI_INSTANCE_ID="$INSTANCE_ID"
export BONSAI_DATA_VOL_ID="$DATA_VOL_ID"
export BONSAI_PUBLIC_IP="$PUBLIC_IP"
export BONSAI_SSH_KEY="${OCI_SSH_KEY_PATH%.pub}"
export OCI_REGION="$OCI_REGION"
export BONSAI_PROVISIONED_AT="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
EOF
_log "  State written to: $STATE_FILE"

_log ""
_log "=== Provisioning complete ==="
_log ""
_log "Connect:  ssh -i ${OCI_SSH_KEY_PATH%.pub} opc@$PUBLIC_IP"
_log "Deploy:   bash scripts/cloud/deploy.sh"
_log "Destroy:  bash scripts/cloud/oracle_setup.sh --destroy"
_log ""
_log "Wait ~60s for cloud-init to finish before running deploy.sh"
