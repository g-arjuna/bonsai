<script>
  let { sidecars, detections } = $props()

  // Aggregate "last fired" timestamp per rule_id from the detections list.
  let lastFiredByRule = $derived.by(() => {
    const map = {}
    for (const d of detections) {
      const t = d.fired_at || d.fired_at_ns || 0
      if (!map[d.rule_id] || t > map[d.rule_id]) map[d.rule_id] = t
    }
    return map
  })

  // Union of all capabilities advertised by registered sidecars.
  let allRules = $derived.by(() => {
    const set = new Set()
    for (const s of sidecars) {
      for (const c of (s.capabilities || [])) set.add(c)
    }
    return Array.from(set).sort()
  })

  function relative(t) {
    if (!t) return 'never'
    const ms = typeof t === 'number' ? (t > 1e15 ? Math.floor(t/1e6) : t) : Date.parse(t)
    const diff = Date.now() - ms
    if (diff < 0) return 'in the future'
    if (diff < 60_000) return `${Math.floor(diff/1000)}s ago`
    if (diff < 3600_000) return `${Math.floor(diff/60_000)}m ago`
    if (diff < 86400_000) return `${Math.floor(diff/3600_000)}h ago`
    return `${Math.floor(diff/86400_000)}d ago`
  }
</script>

{#if allRules.length === 0}
  <p class="empty">No rules advertised. Register a sidecar first.</p>
{:else}
  <table>
    <thead>
      <tr>
        <th>rule_id</th>
        <th>last fired</th>
      </tr>
    </thead>
    <tbody>
      {#each allRules as rule}
        <tr class={lastFiredByRule[rule] ? 'fired' : 'dormant'}>
          <td><code>{rule}</code></td>
          <td>{relative(lastFiredByRule[rule])}</td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<style>
  table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
  thead th {
    text-align: left;
    color: var(--text-dim);
    font-weight: 500;
    text-transform: uppercase;
    font-size: 0.7rem;
    letter-spacing: 0.05em;
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid var(--border);
  }
  tbody td {
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid var(--border);
  }
  tr.dormant { color: var(--text-dim); }
  tr.fired td:first-child code { color: var(--accent); }
  .empty { color: var(--text-dim); }
</style>
