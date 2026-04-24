<script>
  import { onMount } from 'svelte';
  import { navigate } from '$lib/router.svelte.js';
  import { relativeTime, absoluteTime, shortTime, duration } from '$lib/timeutil.js';

  let incidents = $state([]);
  let loading = $state(true);
  let error = $state(null);

  const SEV_CLASS = { critical: 'critical', high: 'warn', warn: 'warn', warning: 'warn', medium: 'info', low: 'info', unknown: 'info' };

  onMount(async () => {
    try {
      const r = await fetch('/api/incidents');
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      incidents = data.incidents ?? [];
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  });

  function sevClass(sev) {
    return SEV_CLASS[sev?.toLowerCase()] ?? 'info';
  }

  function detectionCount(incident) {
    return 1 + (incident.cascading?.length ?? 0);
  }

  function incidentDetections(incident) {
    return [incident.root, ...(incident.cascading ?? [])].filter(Boolean);
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Closed-loop engine</p>
      <h2>Incidents</h2>
    </div>
  </div>

  {#if loading}
    <div class="skeleton-stack">
      {#each [1, 2, 3] as _}
        <div class="card skeleton"></div>
      {/each}
    </div>
  {:else if error}
    <div class="notice error">{error}</div>
  {:else if incidents.length === 0}
    <div class="empty">No incidents recorded yet.</div>
  {:else}
    <div class="incident-list">
      {#each incidents as inc (inc.id ?? inc.root?.id)}
        <div class="card incident-card">
          <div class="incident-header">
            <span class="badge {sevClass(inc.severity)}">{inc.severity ?? 'unknown'}</span>
            <span class="incident-device">{inc.root?.device_address ?? '—'}</span>
            <span class="incident-count">{detectionCount(inc)} event{detectionCount(inc) === 1 ? '' : 's'}</span>
            <span class="muted" title={absoluteTime(inc.started_at_ns)}>
              {relativeTime(inc.started_at_ns)}
            </span>
          </div>

          <div class="incident-summary">
            <div><strong>Root rule:</strong> <code>{inc.root?.rule_id ?? 'unknown'}</code></div>
            <div><strong>Affected devices:</strong> {(inc.affected_devices ?? []).join(', ') || '—'}</div>
            <div><strong>Remediation:</strong> {inc.remediation_status ?? 'none'}</div>
            <div><strong>Window:</strong> {duration(inc.started_at_ns, inc.ended_at_ns) || 'instant'}</div>
          </div>

          {#if incidentDetections(inc).length}
            <div class="detection-timeline">
              {#each incidentDetections(inc).slice(0, 5) as det}
                <button class="det-row"
                        onclick={() => det.id && navigate('/trace/' + encodeURIComponent(det.id))}
                        disabled={!det.id}>
                  <span class="det-ts" title={absoluteTime(det.fired_at_ns)}>{shortTime(det.fired_at_ns)}</span>
                  <span class="det-msg">
                    <strong>{det.device_address ?? 'device'}</strong>
                    {det.rule_id ? ` - ${det.rule_id}` : ''}
                  </span>
                  {#if det.id}
                    <span class="det-link">→ trace</span>
                  {/if}
                </button>
              {/each}
              {#if incidentDetections(inc).length > 5}
                <div class="muted" style="font-size:12px; padding: 4px 0;">
                  +{incidentDetections(inc).length - 5} more
                </div>
              {/if}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .skeleton-stack { display: grid; gap: 12px; }
  .skeleton { height: 80px; background: var(--bg2); border-radius: 6px; opacity: 0.5; animation: pulse 1.5s ease-in-out infinite; }
  @keyframes pulse { 0%, 100% { opacity: 0.5; } 50% { opacity: 0.25; } }

  .incident-list { display: grid; gap: 12px; }

  .incident-card { padding: 14px 16px; }

  .incident-header {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .incident-device { font-weight: 600; }
  .incident-count  { color: var(--muted); font-size: 12px; margin-left: auto; }
  .incident-summary {
    display: grid;
    gap: 6px;
    margin-top: 10px;
    font-size: 13px;
    color: var(--muted);
  }

  .detection-timeline {
    margin-top: 10px;
    border-top: 1px solid var(--border);
    padding-top: 8px;
  }

  .det-row {
    display: flex;
    gap: 10px;
    align-items: center;
    padding: 4px 6px;
    border-radius: 4px;
    cursor: pointer;
    width: 100%;
    background: transparent;
    border: none;
    color: var(--text);
    text-align: left;
    font-size: inherit;
    font-family: inherit;
  }
  .det-row:hover:not(:disabled) { background: rgba(255,255,255,0.04); }
  .det-row:disabled { cursor: default; opacity: 0.7; }

  .det-ts   { color: var(--muted); font-size: 12px; min-width: 90px; font-variant-numeric: tabular-nums; }
  .det-msg  { flex: 1; font-size: 13px; }
  .det-link { color: var(--blue); font-size: 12px; }
</style>
