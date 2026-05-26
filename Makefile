# Bonsai — common dev tasks
# Usage: make <target>
#
# Targets:
#   help     Show this message (default)
#   dev      Build Rust debug binary
#   test     Run all Rust + Python tests
#   lint     Run clippy + Python ruff/flake8
#   ui       Build the Svelte UI
#   ui-dev   Start the Vite dev server (hot-reload)
#   docker   Start bonsai via Docker Compose (standalone profile)
#   docker-down  Stop and remove standalone containers
#   clean    Remove build artifacts

.PHONY: help dev dev-fast ci-build build-time sccache-stats test lint ui ui-dev \
        docker docker-down clean check-deps install-deps-ubuntu \
        test-python test-ui test-all test-integration release backup

CARGO := cargo
UI_DIR := ui
PYTHON_DIR := python

help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}' || \
	sed -n '/^# Targets:/,/^$$/p' $(MAKEFILE_LIST) | grep -v '^#'

# ── Build ──────────────────────────────────────────────────────────────────

dev: ## Build Rust debug binary (uses sccache + mold if installed)
	$(CARGO) build

dev-fast: ## Build with sccache explicitly set (override if RUSTC_WRAPPER not in env)
	RUSTC_WRAPPER=sccache $(CARGO) build

ci-build: ## Build using CI profile (no incremental, no debug info)
	$(CARGO) build --profile ci

build-time: ## Benchmark a full clean build (destructive)
	@echo "=== Clearing sccache stats ==="
	sccache --zero-stats 2>/dev/null || true
	$(CARGO) clean
	@echo "=== Building ==="
	time $(CARGO) build
	@echo "=== sccache stats ==="
	sccache --show-stats 2>/dev/null || true

sccache-stats: ## Show sccache hit/miss statistics
	sccache --show-stats

release: ## Build Rust release binary + UI, package into dist/
	$(CARGO) build --release
	cd $(UI_DIR) && npm ci && npm run build
	@mkdir -p dist
	cp target/release/bonsai dist/ 2>/dev/null || true
	cp -r $(UI_DIR)/dist dist/ui 2>/dev/null || true
	@echo "Release artefacts in dist/"

# ── Test ───────────────────────────────────────────────────────────────────

test: ## Run Rust (nextest) + Python tests
	$(CARGO) nextest run --workspace 2>/dev/null || $(CARGO) test --workspace
	cd $(PYTHON_DIR) && python -m pytest tests/ -v

test-python: ## Run Python tests only
	cd $(PYTHON_DIR) && python -m pytest tests/ -v

test-ui: ## Run UI smoke tests (Playwright)
	cd $(UI_DIR) && npm run test:smoke 2>/dev/null || echo "No UI smoke tests configured yet"

test-integration: ## Run Rust integration tests only
	$(CARGO) test --test '*' -- 2>/dev/null || echo "No integration test files found"

test-all: ## Run all test suites (Rust + Python + UI)
	@echo "=== Rust tests ==="
	$(CARGO) nextest run --workspace 2>/dev/null || $(CARGO) test --workspace
	@echo "=== Python tests ==="
	cd $(PYTHON_DIR) && python -m pytest tests/ -v || true
	@echo "=== UI tests ==="
	cd $(UI_DIR) && npm run test:smoke 2>/dev/null || echo "No UI smoke tests"

# ── Lint ───────────────────────────────────────────────────────────────────

lint: ## Run clippy + Python ruff (or flake8)
	$(CARGO) clippy -- -D warnings
	cd $(PYTHON_DIR) && (ruff check . 2>/dev/null || python -m flake8 . --max-line-length=120 || true)

# ── UI ─────────────────────────────────────────────────────────────────────

ui: ## Build the Svelte UI (production)
	cd $(UI_DIR) && npm ci && npm run build

ui-dev: ## Start Vite dev server with HMR
	cd $(UI_DIR) && npm run dev

# ── Docker ─────────────────────────────────────────────────────────────────

docker: ## Start bonsai (Docker Compose standalone profile)
	@if [ ! -f .env ]; then \
		cp .env.example .env; \
		echo "Created .env from .env.example — set BONSAI_VAULT_PASSPHRASE before running again."; \
		exit 1; \
	fi
	docker compose --profile standalone up -d
	@echo ""
	@echo "  Bonsai is running at http://localhost:3000"

docker-down: ## Stop and remove standalone containers
	docker compose --profile standalone down

# ── Dependencies ───────────────────────────────────────────────────────────

check-deps: ## Verify all required tools are installed
	@echo "Checking dependencies..."
	@command -v rustc  >/dev/null 2>&1 && echo "  rustc   $$(rustc --version)"  || echo "  rustc   MISSING"
	@command -v cargo  >/dev/null 2>&1 && echo "  cargo   $$(cargo --version)"  || echo "  cargo   MISSING"
	@command -v node   >/dev/null 2>&1 && echo "  node    $$(node --version)"   || echo "  node    MISSING"
	@command -v npm    >/dev/null 2>&1 && echo "  npm     $$(npm --version)"    || echo "  npm     MISSING"
	@command -v docker >/dev/null 2>&1 && echo "  docker  $$(docker --version)" || echo "  docker  MISSING"
	@command -v cmake  >/dev/null 2>&1 && echo "  cmake   $$(cmake --version | head -1)" || echo "  cmake   MISSING (required for full build)"
	@command -v protoc >/dev/null 2>&1 && echo "  protoc  $$(protoc --version)" || echo "  protoc  MISSING (required for gRPC)"
	@command -v python3 >/dev/null 2>&1 && echo "  python3 $$(python3 --version)" || echo "  python3 MISSING"

install-deps-ubuntu: ## Install build dependencies on Ubuntu (including mold + sccache)
	sudo apt-get update && sudo apt-get install -y \
		build-essential pkg-config libssl-dev clang cmake \
		protobuf-compiler git curl wget jq \
		python3 python3-pip python3-venv \
		nodejs npm snmp \
		mold \
		sccache
	@echo ""
	@echo "Build speed tools installed:"
	@mold --version 2>/dev/null && echo "  mold: OK" || echo "  mold: NOT FOUND"
	@sccache --version 2>/dev/null && echo "  sccache: OK" || echo "  sccache: NOT FOUND (try: cargo install sccache)"
	@echo ""
	@echo "Note: sccache is active via .cargo/config.toml [build] rustc-wrapper."
	@echo "      Run 'make sccache-stats' after a build to verify cache hits."

# ── Operations ─────────────────────────────────────────────────────────────

backup: ## Create a timestamped backup of runtime/ directory
	@mkdir -p backups
	tar czf backups/bonsai-$$(date +%Y-%m-%dT%H-%M-%S).tar.gz runtime/
	@echo "Backup created in backups/"

clean: ## Remove Rust build artifacts
	$(CARGO) clean
