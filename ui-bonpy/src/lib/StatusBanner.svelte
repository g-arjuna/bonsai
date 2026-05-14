<script>
  let { required_kinds, missing_required, sidecars, last_fetch_ok, last_error } = $props()

  function tone(req, missing, ok) {
    if (!ok) return 'err'
    if (!req || req.length === 0) return 'ok'
    if (missing === null) return 'warn'      // grace window
    if (missing.length === 0) return 'ok'
    return 'err'
  }
  function message(req, missing, sidecars, ok, err) {
    if (!ok) return `Cannot reach /api/sidecars: ${err}`
    if (!req || req.length === 0) return `${sidecars.length} sidecar(s) registered. No kinds required (BONSAI_REQUIRE_SIDECAR unset).`
    if (missing === null) return `Startup grace window in effect. Required: ${req.join(', ')}.`
    if (missing.length === 0) return `All required sidecar kinds present and healthy: ${req.join(', ')}.`
    return `Missing required sidecar kinds: ${missing.join(', ')}. /health reports degraded.`
  }
</script>

<div class="banner {tone(required_kinds, missing_required, last_fetch_ok)}">
  {message(required_kinds, missing_required, sidecars, last_fetch_ok, last_error)}
</div>

<style>
  .banner {
    padding: 0.75rem 1rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    font-weight: 500;
  }
  .ok   { background: rgba(63,185,80,0.08); border-color: var(--ok); color: var(--ok); }
  .warn { background: rgba(210,153,34,0.08); border-color: var(--warn); color: var(--warn); }
  .err  { background: rgba(248,81,73,0.08); border-color: var(--err); color: var(--err); }
</style>
