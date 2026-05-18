<script>
  const { topology, activeSite, onSiteSelect } = $props();

  // Derive per-site summary from topology data — no hardcoded names.
  const siteSummaries = $derived(() => {
    const map = new Map();
    map.set('', { label: 'All sites', count: topology.devices.length, worst: worstHealth(topology.devices) });
    for (const d of topology.devices) {
      const s = d.site || '';
      if (!s) continue;
      if (!map.has(s)) map.set(s, { label: s, count: 0, worst: 'healthy' });
      const entry = map.get(s);
      entry.count++;
      entry.worst = worseThan(entry.worst, d.health);
    }
    return map;
  });

  // Incident overlay: site → worst severity
  const siteIncidents = $derived(() => {
    const map = new Map();
    for (const d of topology.devices) {
      const s = d.site || '';
      const inc = topology.incidentDevices?.get(d.address);
      if (inc) {
        const cur = map.get(s);
        map.set(s, inc === 'critical' ? 'critical' : cur === 'critical' ? 'critical' : 'warn');
      }
    }
    return map;
  });

  function worstHealth(devices) {
    if (devices.some(d => d.health === 'critical')) return 'critical';
    if (devices.some(d => d.health === 'warn'))     return 'warn';
    return 'healthy';
  }

  function worseThan(a, b) {
    const rank = { critical: 2, warn: 1, healthy: 0 };
    return (rank[a] ?? 0) >= (rank[b] ?? 0) ? a : b;
  }

  const HEALTH_DOT = { healthy: '#34d399', warn: '#fbbf24', critical: '#f87171' };
  const INC_COLOR  = { warn: '#fbbf24',    critical: '#f87171' };
</script>

<nav class="site-rail" aria-label="Site navigation">
  <div class="rail-header">Sites</div>

  {#each siteSummaries().entries() as [key, s]}
    {@const inc = siteIncidents().get(key)}
    <button
      class="site-item"
      class:active={activeSite === key}
      onclick={() => onSiteSelect(key)}
    >
      <span class="health-dot" style="background:{HEALTH_DOT[s.worst] ?? '#9ca3af'}"></span>
      <span class="site-label">{s.label}</span>
      <span class="site-count">{s.count}</span>
      {#if inc}
        <span class="inc-badge" style="background:{INC_COLOR[inc]}22; color:{INC_COLOR[inc]}; border-color:{INC_COLOR[inc]}44">
          {inc === 'critical' ? '!' : '⚠'}
        </span>
      {/if}
    </button>
  {/each}
</nav>

<style>
  .site-rail {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 6px;
    border-right: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    min-width: 0;
    overflow-y: auto;
  }

  .rail-header {
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-tertiary, #5f6368);
    padding: 4px 6px 8px;
  }

  .site-item {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 8px;
    border-radius: 5px;
    border: none;
    background: none;
    cursor: pointer;
    text-align: left;
    color: var(--text-secondary, #9aa0a6);
    font-size: 12px;
    transition: background 0.1s, color 0.1s;
    white-space: nowrap;
    overflow: hidden;
  }
  .site-item:hover { background: rgba(255,255,255,0.04); color: var(--text-primary, #e8eaed); }
  .site-item.active { background: rgba(94,234,212,0.08); color: var(--accent-primary, #5eead4); }

  .health-dot {
    width: 6px; height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .site-label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .site-count {
    font-size: 10px;
    color: var(--text-tertiary, #5f6368);
    flex-shrink: 0;
  }

  .inc-badge {
    font-size: 9px;
    padding: 1px 4px;
    border-radius: 3px;
    border: 1px solid;
    flex-shrink: 0;
    font-weight: 700;
  }
</style>
