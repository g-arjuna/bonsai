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

.PHONY: help dev test lint ui ui-dev docker docker-down clean

CARGO := cargo
UI_DIR := ui
PYTHON_DIR := python

help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}' || \
	sed -n '/^# Targets:/,/^$$/p' $(MAKEFILE_LIST) | grep -v '^#'

dev: ## Build Rust debug binary
	$(CARGO) build

release: ## Build Rust release binary
	$(CARGO) build --release

test: ## Run Rust (nextest) + Python tests
	$(CARGO) nextest run --workspace 2>/dev/null || $(CARGO) test --workspace
	cd $(PYTHON_DIR) && python -m pytest tests/ -v

lint: ## Run clippy + Python ruff (or flake8)
	$(CARGO) clippy -- -D warnings
	cd $(PYTHON_DIR) && (ruff check . 2>/dev/null || python -m flake8 . --max-line-length=120 || true)

ui: ## Build the Svelte UI (production)
	cd $(UI_DIR) && npm ci && npm run build

ui-dev: ## Start Vite dev server with HMR
	cd $(UI_DIR) && npm run dev

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

clean: ## Remove Rust build artifacts
	$(CARGO) clean
