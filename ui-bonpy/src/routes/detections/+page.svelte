<script>
  import { createQuery } from '@tanstack/svelte-query';
  import { api } from '$lib/api.js';
  import { lastDetectionEvent } from '$lib/sse.js';

  const detectionsQ = createQuery({
    queryKey: ['detections'],
    queryFn: () => api.detections.list(100),
    refetchInterval: 15000,
  });

  let severityFilter = '';
  let deviceFilter = '';
  let expanded = new Set();

  function sevColor(s) {
    return { critical: 'red', high: 'orange', warning: 'yellow', info: 'gray' }[s] || 'gray';
  }
  function fmt(ns) {
    if (!ns) return '—';
    const d = new Date(ns / 1e6);
    const diff = Date.now() - d;
    if (diff < 60000) return `${Math.round(diff/1000)}s ago`;
    if (diff < 3600000) return `${Math.round(diff/60000)}m ago`;
    return d.toLocaleTimeString();
  }

  function toggle(id) {
    const s = new Set(expanded);
    s.has(id) ? s.delete(id) : s.add(id);
    expanded = s;
  }

  $: liveEvent = $lastDetectionEvent;
  $: rawDetections = $detectionsQ.data?.detections ?? $detectionsQ.data ?? [];
  $: detections = rawDetections
    .filter(d => !severityFilter || d.severity === severityFilter)
    .filter(d => !deviceFilter || d.device_address?.includes(deviceFilter));
</script>

<div class="page">
  <h1 class="page-title">Detection Stream</h1>

  {#if liveEvent}
  <div class="live-event">
    <span class="dot pulse"></span>
    New detection · <strong>{liveEvent.payload?.device_address}</strong>
    · <span class="badge {sevColor(liveEvent.payload?.severity)}">{liveEvent.payload?.severity}</span>
    · {liveEvent.payload?.rule_id}
  </div>
  {/if}

  <div class="filter-row">
    <select bind:value={severityFilter} class="filter-select">
      <option value="">All severities</option>
      <option value="critical">Critical</option>
      <option value="high">High</option>
      <option value="warning">Warning</option>
      <option value="info">Info</option>
    </select>
    <input class="filter-input" bind:value={deviceFilter} placeholder="Filter device…" />
    <span class="muted text-sm">{detections.length} events</span>
  </div>

  {#if $detectionsQ.isLoading}<p class="muted">Loading…</p>
  {:else}
  <table class="table">
    <thead>
      <tr><th>Sev</th><th>Device</th><th>Rule</th><th>Reason</th><th>Time</th><th>GNN</th><th>Cluster</th></tr>
    </thead>
    <tbody>
      {#each detections as d}
      <tr class="clickable" on:click={() => toggle(d.id)}>
        <td><span class="badge {sevColor(d.severity)}">{d.severity}</span></td>
        <td class="mono text-sm">{d.device_address}</td>
        <td class="mono text-sm">{d.rule_id}</td>
        <td class="reason">{d.reason?.slice(0, 60)}{d.reason?.length > 60 ? '…' : ''}</td>
        <td class="muted text-sm">{fmt(d.occurred_at_ns)}</td>
        <td class="mono text-sm">{d.gnn_score?.toFixed(2) ?? '—'}</td>
        <td class="mono text-sm muted">{d.incident_cluster_id || '—'}</td>
      </tr>
      {#if expanded.has(d.id)}
      <tr class="expand-row">
        <td colspan="7">
          <div class="expand-panel">
            <div class="expand-grid">
              <div><span class="dlabel">Full reason</span><span>{d.reason}</span></div>
              {#if d.features_json}<div><span class="dlabel">Features</span><span class="mono text-sm pre">{d.features_json?.slice(0,200)}</span></div>{/if}
              {#if d.investigation_id}<div><a href="/investigations/{d.investigation_id}" class="inv-link">View Investigation →</a></div>{/if}
              {#if d.remediation_id}<div><a href="/remediations/{d.remediation_id}" class="inv-link">View Remediation →</a></div>{/if}
            </div>
          </div>
        </td>
      </tr>
      {/if}
      {/each}
    </tbody>
  </table>
  {/if}
</div>

<style>
  .page-title { font-size: 20px; font-weight: 700; margin: 0 0 16px; }
  .live-event { display: flex; align-items: center; gap: 8px; background: rgba(248,81,73,0.06); border: 1px solid rgba(248,81,73,0.2); border-radius: 6px; padding: 8px 14px; font-size: 13px; margin-bottom: 14px; }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: #f85149; flex-shrink: 0; }
  .dot.pulse { animation: pulse 1.5s infinite; }
  @keyframes pulse { 0%,100% { opacity:1; } 50% { opacity:0.3; } }
  .filter-row { display: flex; align-items: center; gap: 10px; margin-bottom: 14px; }
  .filter-select, .filter-input { background: var(--bg-surface, #161b22); border: 1px solid var(--border, #30363d); color: var(--text-primary, #e6edf3); border-radius: 5px; padding: 5px 10px; font-size: 13px; }
  .table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .table th { text-align: left; padding: 6px 8px; border-bottom: 1px solid var(--border, #30363d); color: var(--text-secondary, #8b949e); font-size: 11px; text-transform: uppercase; }
  .table td { padding: 7px 8px; border-bottom: 1px solid var(--border, #30363d); }
  .clickable { cursor: pointer; }
  .clickable:hover td { background: var(--bg-hover, #21262d); }
  .mono { font-family: 'JetBrains Mono', monospace; }
  .muted { color: var(--text-secondary, #8b949e); }
  .text-sm { font-size: 12px; }
  .reason { max-width: 260px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .badge { padding: 2px 6px; border-radius: 4px; font-size: 11px; font-weight: 600; }
  .badge.red    { background: rgba(248,81,73,0.15);  color: #f85149; }
  .badge.orange { background: rgba(210,153,34,0.15); color: #d29922; }
  .badge.yellow { background: rgba(210,153,34,0.1);  color: #e3b341; }
  .badge.gray   { background: rgba(139,148,158,0.1); color: #8b949e; }
  .expand-row td { background: var(--bg-hover, #21262d); padding: 0; }
  .expand-panel { padding: 14px 16px; }
  .expand-grid { display: flex; flex-direction: column; gap: 8px; font-size: 12px; }
  .dlabel { display: block; color: var(--text-secondary, #8b949e); font-size: 11px; margin-bottom: 2px; }
  .pre { white-space: pre-wrap; word-break: break-all; }
  .inv-link { color: var(--accent-primary, #4f8ef7); text-decoration: none; font-size: 12px; }
</style>
