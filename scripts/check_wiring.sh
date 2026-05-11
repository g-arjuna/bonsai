#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

pass() {
  echo "PASS: $*"
}

grep -q "MultiSourceEnricherRegistry::from_layered_config" src/change_detection.rs \
  || fail "change detection does not build the multi-source enricher registry"
grep -q "\.capture(&target, resolved_credentials.as_ref())" src/change_detection.rs \
  || fail "change detection does not dispatch capture requests through the registry"
grep -q "ParserChainCliEnricher" src/enrichment/registry.rs \
  || fail "registry does not include the parser-chain CLI enricher"
grep -q "ParseRequest" src/enrichment/parser_chain_enricher.rs \
  || fail "parser-chain CLI enricher does not invoke ParserChain"
grep -q "scripts/cli_capture.py" src/enrichment/parser_chain_enricher.rs \
  || fail "parser-chain CLI enricher is not wired to the CLI capture helper"
grep -q '"/api/devices/{address}/gnmi-readiness"' src/http_server.rs \
  || fail "gNMI readiness endpoint route missing"
grep -q '"/api/devices/{address}/recommendations"' src/http_server.rs \
  || fail "recommendations endpoint route missing"
grep -q '"/api/devices/{address}/config-history"' src/http_server.rs \
  || fail "config history endpoint route missing"
grep -q '"/api/yang/modules"' src/http_server.rs \
  || fail "YANG modules endpoint route missing"
grep -q '"/api/yang/search"' src/http_server.rs \
  || fail "YANG search endpoint route missing"

pass "Sprint 1 wiring checks passed"
