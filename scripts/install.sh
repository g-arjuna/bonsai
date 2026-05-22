#!/usr/bin/env bash
# Bonsai bootstrap installer — DV3 D3-1 T1
#
# One-command install on Linux (x86-64/arm64) or macOS (Apple Silicon/Intel).
# Primary path: Docker Compose --profile standalone (no ContainerLab required).
# Fallback path: build from source if Docker is unavailable.
#
# Usage (new machine):
#   curl -sSf https://raw.githubusercontent.com/arjuna/bonsai/main/scripts/install.sh | bash
#
# Or from a local clone:
#   bash scripts/install.sh [--source] [--no-open]
#
#   --source    Force build from source even if Docker is available.
#   --no-open   Do not open the browser after startup.
#   --containerlab  Also install ContainerLab (Linux only).
#   --uninstall     Remove bonsai binary, systemd units, and optionally containers.
#   --help      Show this message.

set -euo pipefail

# ── Colours ────────────────────────────────────────────────────────────────────
RED='\033[0;31m'; YELLOW='\033[1;33m'; GREEN='\033[0;32m'; BOLD='\033[1m'; NC='\033[0m'
info()    { echo -e "${GREEN}[bonsai]${NC} $*"; }
warn()    { echo -e "${YELLOW}[bonsai]${NC} $*"; }
error()   { echo -e "${RED}[bonsai]${NC} $*" >&2; }
step()    { echo -e "\n${BOLD}── $* ──${NC}"; }
die()     { error "$*"; exit 1; }

# ── Flags ─────────────────────────────────────────────────────────────────────
FORCE_SOURCE=false
NO_OPEN=false
INSTALL_CLAB=false
DO_UNINSTALL=false
while [[ $# -gt 0 ]]; do
    case "$1" in
        --source)       FORCE_SOURCE=true ;;
        --no-open)      NO_OPEN=true ;;
        --containerlab) INSTALL_CLAB=true ;;
        --uninstall)    DO_UNINSTALL=true ;;
        --help|-h)
            sed -n '2,20p' "$0" | grep '^#' | sed 's/^# \?//'
            exit 0
            ;;
        *) die "Unknown argument: $1. Run with --help for usage." ;;
    esac
    shift
done

# ── Uninstall ─────────────────────────────────────────────────────────────────
if $DO_UNINSTALL; then
    step "Uninstalling bonsai"
    if command -v docker &>/dev/null && docker compose version &>/dev/null 2>&1; then
        info "Stopping Docker Compose services..."
        docker compose --profile standalone down 2>/dev/null || true
    fi
    for UNIT in bonsai bonsai-chaos bonsai-rules-sidecar; do
        if systemctl is-enabled "$UNIT" &>/dev/null 2>&1; then
            info "Disabling systemd unit $UNIT..."
            sudo systemctl stop "$UNIT" 2>/dev/null || true
            sudo systemctl disable "$UNIT" 2>/dev/null || true
        fi
    done
    for BIN in /usr/local/bin/bonsai "$HOME/.local/bin/bonsai"; do
        if [[ -f "$BIN" ]]; then
            info "Removing $BIN"
            rm -f "$BIN" 2>/dev/null || sudo rm -f "$BIN" 2>/dev/null || true
        fi
    done
    info "Uninstall complete. Repository and config files are preserved."
    exit 0
fi

# ── Detect OS / arch ─────────────────────────────────────────────────────────
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
    Linux)  PLATFORM="linux" ;;
    Darwin) PLATFORM="macos" ;;
    *)      die "Unsupported OS: $OS. Bonsai runs on Linux and macOS." ;;
esac
case "$ARCH" in
    x86_64|amd64)   ARCH_TAG="x86_64" ;;
    arm64|aarch64)  ARCH_TAG="aarch64" ;;
    *)              die "Unsupported architecture: $ARCH." ;;
esac
info "Platform: ${PLATFORM}/${ARCH_TAG}"

# ── Dependency version checks (D4-22 T3) ───────────────────────────────────────
version_ge() {
    # Returns 0 (true) if $1 >= $2 using sort -V
    printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1 | grep -qxF "$2"
}

check_rust_version() {
    if command -v rustc &>/dev/null; then
        local ver
        ver="$(rustc --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')"
        if ! version_ge "$ver" "1.70.0"; then
            warn "Rust $ver found but >= 1.70.0 recommended. Consider: rustup update stable"
        fi
    fi
}

check_docker_version() {
    if command -v docker &>/dev/null; then
        local ver
        ver="$(docker version --format '{{.Server.Version}}' 2>/dev/null || echo '0.0.0')"
        if ! version_ge "$ver" "24.0.0"; then
            warn "Docker $ver found but >= 24.0 recommended for BuildKit support."
        fi
    fi
}

check_rust_version
check_docker_version

# ── Idempotency check ─────────────────────────────────────────────────────
if command -v bonsai &>/dev/null; then
    INSTALLED_VER="$(bonsai --version 2>/dev/null || echo 'unknown')"
    info "Bonsai already installed ($INSTALLED_VER). Re-running will upgrade in place."
fi

# ── Locate repo root ──────────────────────────────────────────────────────────
# When piped through bash the script may run from a temp file. Try to find
# an existing repo clone, otherwise clone into ~/bonsai.
if [[ -f "$(pwd)/Cargo.toml" ]] && grep -q 'name = "bonsai"' "$(pwd)/Cargo.toml" 2>/dev/null; then
    REPO_ROOT="$(pwd)"
elif [[ -f "$(dirname "${BASH_SOURCE[0]:-$0}")/../Cargo.toml" ]]; then
    REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
else
    step "Cloning bonsai repository"
    if ! command -v git &>/dev/null; then
        die "git is required to clone the repository. Install git and re-run."
    fi
    REPO_ROOT="$HOME/bonsai"
    if [[ -d "$REPO_ROOT/.git" ]]; then
        info "Repository already exists at $REPO_ROOT — pulling latest."
        git -C "$REPO_ROOT" pull --ff-only
    else
        git clone https://github.com/arjuna/bonsai "$REPO_ROOT"
    fi
fi
info "Repository: $REPO_ROOT"
cd "$REPO_ROOT"

# ── Choose install path ───────────────────────────────────────────────────────
HAS_DOCKER=false
if command -v docker &>/dev/null && docker info &>/dev/null 2>&1; then
    HAS_DOCKER=true
fi

if $FORCE_SOURCE || ! $HAS_DOCKER; then
    INSTALL_PATH="source"
    if $FORCE_SOURCE; then
        info "Source build requested (--source)."
    else
        warn "Docker not found or not running — falling back to source build."
    fi
else
    INSTALL_PATH="docker"
fi

# ── Vault passphrase ──────────────────────────────────────────────────────────
# Read from existing .env, or prompt the user.
setup_passphrase() {
    if [[ -f "$REPO_ROOT/.env" ]] && grep -q "^BONSAI_VAULT_PASSPHRASE=." "$REPO_ROOT/.env"; then
        info "BONSAI_VAULT_PASSPHRASE already set in .env."
        return
    fi
    echo ""
    echo -e "${BOLD}Vault passphrase${NC}"
    echo "  Bonsai encrypts all device credentials (gNMI, SNMP, API keys) in a"
    echo "  local vault file (runtime/vault.age) using age encryption + HMAC-SHA256."
    echo ""
    echo "  Requirements:"
    echo "    • Minimum 12 characters (longer is better)."
    echo "    • Avoid dictionary words — use a password manager or the auto-generate option."
    echo ""
    echo "  ${YELLOW}WARNING: If the passphrase is lost, the vault is unrecoverable.${NC}"
    echo "    • Back up the vault.age file before any re-key operation."
    echo "    • To change the passphrase later:"
    echo "        BONSAI_VAULT_PASSPHRASE=<old> BONSAI_VAULT_NEW_PASSPHRASE=<new> vault-rekey"
    echo "      or via the API: POST /api/vault/rekey {\"new_passphrase_env\": \"BONSAI_VAULT_NEW_PASSPHRASE\"}"
    echo ""
    echo "  Choose a strong passphrase — you will need it on every restart."
    echo "  (Press Enter to generate a random one automatically.)"
    echo ""
    read -r -s -p "  Passphrase [auto-generate]: " PASS
    echo ""
    if [[ -z "$PASS" ]]; then
        PASS="$(openssl rand -base64 24 2>/dev/null || dd if=/dev/urandom bs=18 count=1 2>/dev/null | base64)"
        info "Generated passphrase: ${BOLD}${PASS}${NC}"
        warn "IMPORTANT: save this passphrase — you cannot recover your vault without it."
    fi

    if [[ ! -f "$REPO_ROOT/.env" ]]; then
        cp "$REPO_ROOT/.env.example" "$REPO_ROOT/.env"
        info "Created .env from .env.example."
    fi
    # Write or replace the passphrase line
    if grep -q "^BONSAI_VAULT_PASSPHRASE=" "$REPO_ROOT/.env"; then
        sed -i.bak "s|^BONSAI_VAULT_PASSPHRASE=.*|BONSAI_VAULT_PASSPHRASE=${PASS}|" "$REPO_ROOT/.env" && rm -f "$REPO_ROOT/.env.bak"
    else
        echo "BONSAI_VAULT_PASSPHRASE=${PASS}" >> "$REPO_ROOT/.env"
    fi
    info "Passphrase written to .env."
}

# ── Path A: Docker Compose standalone ────────────────────────────────────────
install_docker() {
    step "Docker Compose install (standalone profile)"

    # Docker Compose v2 check
    if ! docker compose version &>/dev/null 2>&1; then
        die "Docker Compose v2 is required. Install it from https://docs.docker.com/compose/install/"
    fi

    setup_passphrase

    step "Building bonsai image"
    docker compose build bonsai-standalone

    step "Starting bonsai"
    docker compose --profile standalone up -d

    step "Waiting for healthcheck"
    local MAX_WAIT=60
    local WAITED=0
    until docker compose ps bonsai-standalone 2>/dev/null | grep -q "healthy"; do
        if [[ $WAITED -ge $MAX_WAIT ]]; then
            warn "Healthcheck did not pass in ${MAX_WAIT}s — bonsai may still be starting."
            warn "Run: docker compose logs bonsai-standalone"
            break
        fi
        printf '.'
        sleep 2
        WAITED=$((WAITED + 2))
    done
    echo ""

    print_success_docker
}

# ── Path B: Build from source ─────────────────────────────────────────────────
install_source() {
    step "Build from source"

    # Rust toolchain
    if ! command -v cargo &>/dev/null; then
        info "Rust toolchain not found — installing via rustup."
        if ! command -v curl &>/dev/null; then
            die "curl is required to install rustup. Install curl and re-run."
        fi
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
        # shellcheck source=/dev/null
        source "$HOME/.cargo/env"
    fi
    info "Rust: $(rustc --version)"

    # Node.js for UI build
    if ! command -v node &>/dev/null; then
        warn "Node.js not found — UI will not be built. Install Node.js 20+ to include the UI."
        BUILD_UI=false
    else
        BUILD_UI=true
        info "Node.js: $(node --version)"
    fi

    step "Building bonsai binary"
    cargo build --release --bin bonsai

    if $BUILD_UI; then
        step "Building UI"
        (cd ui && npm ci && npm run build)
    fi

    step "Installing binary"
    sudo install -m 755 target/release/bonsai /usr/local/bin/bonsai \
        && info "Installed to /usr/local/bin/bonsai." \
        || { cp target/release/bonsai "$HOME/.local/bin/bonsai" 2>/dev/null \
             && info "Installed to ~/.local/bin/bonsai." \
             || warn "Could not install to PATH — run ./target/release/bonsai manually."; }

    step "Creating bonsai.toml"
    if [[ ! -f "$REPO_ROOT/bonsai.toml" ]]; then
        cp "$REPO_ROOT/bonsai.toml.example" "$REPO_ROOT/bonsai.toml"
        info "Created bonsai.toml from example."
    else
        info "bonsai.toml already exists — not overwriting."
    fi

    setup_passphrase
    # Write passphrase to shell profile for convenience
    SHELL_RC="$HOME/.bashrc"
    [[ "$SHELL" == */zsh ]] && SHELL_RC="$HOME/.zshrc"
    if ! grep -q "BONSAI_VAULT_PASSPHRASE" "$SHELL_RC" 2>/dev/null; then
        PASS_VAL="$(grep "^BONSAI_VAULT_PASSPHRASE=" "$REPO_ROOT/.env" | cut -d= -f2-)"
        echo "export BONSAI_VAULT_PASSPHRASE='${PASS_VAL}'" >> "$SHELL_RC"
        info "Added BONSAI_VAULT_PASSPHRASE to $SHELL_RC."
    fi

    print_success_source
}

# ── Success messages ──────────────────────────────────────────────────────────
print_success_docker() {
    echo ""
    echo -e "${GREEN}${BOLD}────────────────────────────────────────────────────${NC}"
    echo -e "${GREEN}${BOLD}  Bonsai is running!${NC}"
    echo -e "${GREEN}${BOLD}────────────────────────────────────────────────────${NC}"
    echo ""
    echo "  UI:    http://localhost:3000"
    echo "  API:   http://localhost:3000/api"
    echo "  Docs:  http://localhost:3000/api/docs"
    echo ""
    echo "  Add your first device via the onboarding wizard."
    echo ""
    echo "  Useful commands:"
    echo "    docker compose logs -f bonsai-standalone   # tail logs"
    echo "    docker compose --profile standalone stop    # stop"
    echo "    docker compose --profile standalone down    # stop + remove containers"
    echo ""
    open_browser "http://localhost:3000"
}

print_success_source() {
    PASS_VAL="$(grep "^BONSAI_VAULT_PASSPHRASE=" "$REPO_ROOT/.env" | cut -d= -f2-)"
    echo ""
    echo -e "${GREEN}${BOLD}────────────────────────────────────────────────────${NC}"
    echo -e "${GREEN}${BOLD}  Bonsai installed!${NC}"
    echo -e "${GREEN}${BOLD}────────────────────────────────────────────────────${NC}"
    echo ""
    echo "  Start bonsai:"
    echo "    cd $REPO_ROOT"
    echo "    BONSAI_VAULT_PASSPHRASE='${PASS_VAL}' bonsai --config bonsai.toml"
    echo ""
    echo "  Then open: http://localhost:3000"
    echo ""
}

open_browser() {
    if $NO_OPEN; then return; fi
    local URL="$1"
    if command -v xdg-open &>/dev/null; then
        xdg-open "$URL" &>/dev/null &
    elif command -v open &>/dev/null; then
        open "$URL" &>/dev/null &
    fi
}

# ── Main ──────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}Bonsai — Network Intelligence Platform${NC}"
echo "Version: $(git -C "$REPO_ROOT" describe --tags --always 2>/dev/null || echo 'dev')"
echo ""

case "$INSTALL_PATH" in
    docker) install_docker ;;
    source) install_source ;;
esac

# ── Optional: ContainerLab install (D4-22 T3) ─────────────────────────────────
if $INSTALL_CLAB; then
    if [[ "$PLATFORM" != "linux" ]]; then
        warn "ContainerLab is only supported on Linux — skipping."
    elif command -v containerlab &>/dev/null; then
        info "ContainerLab already installed: $(containerlab version 2>/dev/null | head -1)"
    else
        step "Installing ContainerLab"
        bash -c "$(curl -sL https://get.containerlab.dev)" || warn "ContainerLab install failed — install manually from https://containerlab.dev"
    fi
fi
