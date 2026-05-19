<script>
  const { topology, incidentCount, sseConnected, lastRefresh } = $props();

  const deviceCount   = $derived(topology.devices?.length ?? 0);
  const healthyCount  = $derived(topology.devices?.filter(d => d.health === 'healthy').length ?? 0);
  const warnCount     = $derived(topology.devices?.filter(d => d.health === 'warn').length ?? 0);
  const criticalCount = $derived(topology.devices?.filter(d => d.health === 'critical').length ?? 0);

  let haStatus = $state(null);
  let haLoading = $state(true);

  async function loadHAStatus() {
    try {
      const res = await fetch('/api/ha/status');
      if (res.ok) {
        haStatus = await res.json();
      }
    } catch (_) {
      // HA not configured or endpoint unavailable
    } finally {
      haLoading = false;
    }
  }

  loadHAStatus();
  const haInterval = setInterval(loadHAStatus, 30000);

  function fmtAge(ts) {
    if (!ts) return '—';
    const s = Math.round((Date.now() - ts) / 1000);
    if (s < 5)  return 'just now';
    if (s < 60) return `${s}s ago`;
    return `${Math.round(s / 60)}m ago`;
  }
</script>

<div class="status-bar" role="status" aria-live="polite">
  <div class="stat-group">
    <span class="stat-label">Devices</span>
    <span class="stat-value">{deviceCount}</span>
  </div>

  <div class="divider"></div>

  <div class="stat-group">
    {#if criticalCount}
      <span class="health-pill critical">{criticalCount} critical</span>
    {/if}
    {#if warnCount}
      <span class="health-pill warn">{warnCount} warn</span>
    {/if}
    {#if !criticalCount && !warnCount}
      <span class="health-pill healthy">{healthyCount} healthy</span>
    {/if}
  </div>

  <div class="divider"></div>

  <div class="stat-group">
    <span class="stat-label">Incidents</span>
    <span class="stat-value" class:has-incidents={incidentCount > 0}>{incidentCount}</span>
  </div>

  <div class="spacer"></div>

  {#if haStatus && haStatus.mode !== 'standalone'}
    <div class="divider"></div>
    <div class="stat-group ha-indicator" title={haStatus.is_leader ? 'HA: This node is the leader' : `HA: Follower of ${haStatus.leader_id || 'unknown'}`}>
      <span class="ha-dot" class:leader={haStatus.is_leader} class:follower={!haStatus.is_leader}></span>
      <span class="stat-label" style="font-size:11px">{haStatus.is_leader ? 'Leader' : 'Follower'}</span>
    </div>
  {/if}

  <div class="stat-group">
    <span class="sse-dot" class:connected={sseConnected} title={sseConnected ? 'Live stream connected' : 'Reconnecting…'}></span>
    <span class="stat-label" style="font-size:11px">{sseConnected ? 'Live' : 'Reconnecting…'}</span>
  </div>

  <div class="divider"></div>

  <div class="stat-group">
    <span class="stat-label">Updated</span>
    <span class="stat-value age">{fmtAge(lastRefresh)}</span>
  </div>
</div>

<style>
  .status-bar {
    display: flex;
    align-items: center;
    gap: 0;
    height: 32px;
    padding: 0 12px;
    border-bottom: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    background: var(--bg-base, #0a0b0d);
    font-size: 12px;
    flex-shrink: 0;
  }

  .stat-group {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 0 10px;
  }

  .stat-label {
    color: var(--text-tertiary, #5f6368);
    font-size: 11px;
  }

  .stat-value {
    color: var(--text-primary, #e8eaed);
    font-weight: 600;
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }

  .stat-value.has-incidents { color: var(--state-degraded, #fbbf24); }

  .stat-value.age { color: var(--text-secondary, #9aa0a6); font-weight: 400; }

  .divider {
    width: 1px;
    height: 16px;
    background: var(--border-subtle, rgba(255,255,255,0.06));
    flex-shrink: 0;
  }

  .spacer { flex: 1; }

  .health-pill {
    font-size: 11px;
    padding: 1px 7px;
    border-radius: 10px;
    font-weight: 500;
  }
  .health-pill.healthy  { background: rgba(52,211,153,0.12);  color: #34d399; }
  .health-pill.warn     { background: rgba(251,191,36,0.12);  color: #fbbf24; }
  .health-pill.critical { background: rgba(248,113,113,0.12); color: #f87171; }

  .sse-dot {
    width: 7px; height: 7px;
    border-radius: 50%;
    background: var(--text-tertiary, #5f6368);
    transition: background 0.3s;
    flex-shrink: 0;
  }
  .sse-dot.connected {
    background: #34d399;
    box-shadow: 0 0 5px rgba(52,211,153,0.5);
    animation: pulse 2s ease-in-out infinite;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50%       { opacity: 0.5; }
  }

  .ha-indicator {
    cursor: pointer;
  }

  .ha-dot {
    width: 7px; height: 7px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .ha-dot.leader {
    background: #34d399;
    box-shadow: 0 0 5px rgba(52,211,153,0.5);
    animation: pulse 2s ease-in-out infinite;
  }
  .ha-dot.follower {
    background: #fbbf24;
  }
</style>
