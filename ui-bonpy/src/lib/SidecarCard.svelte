<script>
  let { sidecar } = $props()

  function relativeTime(ns) {
    if (!ns) return 'never'
    const ms = Number(BigInt(ns) / 1000000n)
    const diff = Date.now() - ms
    if (diff < 0) return 'in the future'
    if (diff < 60_000) return `${Math.floor(diff/1000)}s ago`
    if (diff < 3600_000) return `${Math.floor(diff/60_000)}m ago`
    return `${Math.floor(diff/3600_000)}h ago`
  }
</script>

<div class="card status-{sidecar.status}">
  <header>
    <div>
      <h3>{sidecar.name}</h3>
      <span class="kind">{sidecar.kind}</span>
    </div>
    <span class="status-pill {sidecar.status}">{sidecar.status}</span>
  </header>

  <dl>
    <dt>version</dt><dd>{sidecar.version}</dd>
    <dt>address</dt><dd><code>{sidecar.address || '—'}</code></dd>
    <dt>last heartbeat</dt><dd>{relativeTime(sidecar.last_heartbeat_ns)}</dd>
    <dt>registered</dt><dd>{relativeTime(sidecar.registered_at_ns)}</dd>
    <dt>events in</dt><dd>{sidecar.events_in_total.toLocaleString()}</dd>
    <dt>detections out</dt><dd>{sidecar.detections_out_total.toLocaleString()}</dd>
  </dl>

  {#if sidecar.capabilities && sidecar.capabilities.length}
    <div class="caps">
      {#each sidecar.capabilities as cap}
        <span class="chip">{cap}</span>
      {/each}
    </div>
  {/if}
</div>

<style>
  .card {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  .card.status-healthy { border-color: var(--ok); }
  .card.status-stale   { border-color: var(--warn); }
  .card.status-lost    { border-color: var(--err); opacity: 0.7; }
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 0.5rem;
  }
  .kind {
    display: inline-block;
    margin-top: 0.25rem;
    font-size: 0.75rem;
    color: var(--text-dim);
    font-family: var(--font-mono);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .status-pill {
    font-size: 0.7rem;
    padding: 2px 8px;
    border-radius: 999px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-weight: 600;
  }
  .status-pill.healthy { background: rgba(63,185,80,0.15); color: var(--ok); }
  .status-pill.stale   { background: rgba(210,153,34,0.15); color: var(--warn); }
  .status-pill.lost    { background: rgba(248,81,73,0.15); color: var(--err); }
  dl {
    display: grid;
    grid-template-columns: 8rem 1fr;
    gap: 0.25rem 0.75rem;
    margin: 0;
    font-size: 0.85rem;
  }
  dt { color: var(--text-dim); }
  dd { margin: 0; }
  .caps {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
    margin-top: 0.25rem;
  }
  .chip {
    font-family: var(--font-mono);
    font-size: 0.7rem;
    background: var(--bg);
    border: 1px solid var(--border);
    padding: 2px 6px;
    border-radius: 3px;
    color: var(--text-dim);
  }
</style>
