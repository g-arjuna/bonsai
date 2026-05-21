<script>
  import { onMount } from 'svelte';
  import { navigate } from '$lib/router.svelte.js';
  import { relativeTime, absoluteTime, shortTime, duration } from '$lib/timeutil.js';

  let incidents = $state([]);
  let loading = $state(true);
  let error = $state(null);
  let expanded = $state(new Set());

  const SEV_CLASS = { critical: 'critical', high: 'warn', warn: 'warn', warning: 'warn', medium: 'info', low: 'info', unknown: 'neutral' };

  const SSE_REFRESH_TYPES = new Set([
    'detection_fired', 'incident_grouped', 'remediation_outcome',
  ]);

  onMount(() => {
    loadIncidents();

    let es;
    try {
      es = new EventSource('/api/events');
      es.onmessage = (e) => {
        try {
          const ev = JSON.parse(e.data);
          if (SSE_REFRESH_TYPES.has(ev.event_type)) loadIncidents();
        } catch {}
      };
    } catch {}

    const poll = setInterval(loadIncidents, 60_000);
    return () => { clearInterval(poll); if (es) es.close(); };
  });

  async function loadIncidents() {
    try {
      const r = await fetch('/api/incidents');
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      incidents = data.incidents ?? [];
      error = null;
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function sevClass(sev) {
    return SEV_CLASS[sev?.toLowerCase()] ?? 'neutral';
  }

  function detectionCount(inc) {
    return 1 + (inc.cascading?.length ?? 0);
  }

  function incidentDetections(inc) {
    return [inc.root, ...(inc.cascading ?? [])].filter(Boolean);
  }

  function incidentKey(inc) {
    return inc.id ?? inc.root?.id ?? JSON.stringify(inc);
  }

  function toggleExpand(key) {
    const next = new Set(expanded);
    if (next.has(key)) next.delete(key); else next.add(key);
    expanded = next;
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Closed-loop engine</p>
      <h2>Incidents</h2>
    </div>
    {#if !loading && incidents.length > 0}
      <span class="open-count">{incidents.length} open</span>
    {/if}
  </div>

  {#if loading}
    <div class="skeleton-stack">
      {#each [1, 2, 3] as _}
        <div class="inc-skeleton"></div>
      {/each}
    </div>
  {:else if error}
    <div class="notice error">{error}</div>
  {:else if incidents.length === 0}
    <div class="empty">
      No incidents recorded yet. Live health changes become incidents after DetectionEvent rows are created.
    </div>
  {:else}
    <div class="incident-list">
      {#each incidents as inc (incidentKey(inc))}
        {@const key = incidentKey(inc)}
        {@const isOpen = expanded.has(key)}
        {@const sev = inc.severity?.toLowerCase() ?? 'unknown'}
        {@const count = detectionCount(inc)}

        <!-- svelte-ignore a11y_no_noninteractive_element_interactions a11y_no_noninteractive_tabindex -->
        <article
          class="inc-card sev-{sevClass(sev)}"
          onclick={() => toggleExpand(key)}
          tabindex="0"
          onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && toggleExpand(key)}
        >
          <!-- Left severity stripe — no icon, color carries the signal -->
          <div class="sev-stripe" aria-label="severity: {sev}"></div>

          <div class="inc-body">
            <!-- Primary row: device + rule pills + spacer + count + age -->
            <div class="row-primary">
              <code class="device-addr">{inc.root?.device_address ?? '—'}</code>
              <span class="rule-pills">
                {#each (inc.rule_ids?.length ? inc.rule_ids : [inc.root?.rule_id ?? 'unknown']) as rid}
                  <span class="rule-pill">{rid}</span>
                {/each}
              </span>
              <span class="spacer"></span>
              <span class="ev-count">{inc.event_count ?? count} event{(inc.event_count ?? count) !== 1 ? 's' : ''}</span>
              <time class="inc-age" title={absoluteTime(inc.started_at_ns)}>
                {relativeTime(inc.started_at_ns)}
              </time>
            </div>

            <!-- Secondary row: clubbing rationale + remediation tag + duration -->
            <div class="row-secondary">
              <span
                class="context-line"
                title={inc.co_fire_signature ?? ''}
              >
                {#if (inc.device_count ?? (inc.affected_devices ?? []).length) > 1}
                  {inc.device_count ?? inc.affected_devices.length} devices · {inc.co_fire_signature ?? ''}
                {:else if inc.co_fire_signature}
                  {inc.co_fire_signature}
                {:else}
                  {inc.root?.device_address ?? ''}
                {/if}
              </span>
              <span class="tag rem-{inc.remediation_status ?? 'none'}">
                {inc.remediation_status ?? 'none'}
              </span>
              <span class="tag">
                {duration(inc.started_at_ns, inc.ended_at_ns) || 'instant'}
              </span>
            </div>
            <!-- Config correlation hint (D2-4 T5) -->
            {#if (inc.rule_ids ?? []).includes('config_caused_fault')}
              {@const cfDet = incidentDetections(inc).find(d => d?.rule_id === 'config_caused_fault')}
              {@const lagMs = cfDet?.features_json ? (() => { try { return JSON.parse(cfDet.features_json)?.detail?.config_lag_ms; } catch { return null; } })() : null}
              <div class="config-hint">
                ⚙ Config change preceded this incident{lagMs != null ? ` by ${lagMs}ms` : ''} — possible operator-caused fault
              </div>
            {/if}

            <!-- Expanded: correlation chain + blast radius + detection timeline -->
            {#if isOpen}
              <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
              <div class="expanded-body" role="presentation" onclick={(e) => e.stopPropagation()}>

                <!-- Provenance: correlation chain -->
                {#if inc.correlation_chain?.length}
                  <div class="provenance-section">
                    <span class="provenance-label">Triggered by</span>
                    {#each inc.correlation_chain as step}
                      <span class="chain-step">
                        <span class="chain-src src-{step.source_type}">{step.source_type}</span>
                        <span class="chain-type">{step.event_type}</span>
                        <code class="chain-dev">{step.device_address}</code>
                      </span>
                    {/each}
                  </div>
                {/if}

                <!-- Blast radius summary -->
                {#if inc.blast_radius_summary}
                  <div class="blast-summary">
                    <span class="blast-chip">
                      ⚡ {inc.blast_radius_summary.device_count} reachable device{inc.blast_radius_summary.device_count === 1 ? '' : 's'}
                    </span>
                    {#if inc.blast_radius_summary.app_count > 0}
                      <span class="blast-chip">
                        📦 {inc.blast_radius_summary.app_count} app{inc.blast_radius_summary.app_count === 1 ? '' : 's'} at risk
                      </span>
                    {/if}
                  </div>
                {/if}

                {#each incidentDetections(inc).slice(0, 8) as det}
                  <button
                    class="det-row"
                    onclick={() => det.id && navigate('/trace/' + encodeURIComponent(det.id))}
                    disabled={!det.id}
                  >
                    <span class="det-ts">{shortTime(det.fired_at_ns)}</span>
                    <code class="det-device">{det.device_address ?? '—'}</code>
                    <span class="det-rule">{det.rule_id ?? ''}</span>
                    {#if det.source_types?.length}
                      <span class="det-sources">
                        {#each det.source_types as st}
                          <span class="src-pill">{st}</span>
                        {/each}
                      </span>
                    {/if}
                    {#if det.latency_ns > 0}
                      <span class="det-latency">{(det.latency_ns / 1e6).toFixed(0)}ms</span>
                    {/if}
                    {#if det.id}<span class="det-trace">trace →</span>{/if}
                  </button>
                {/each}
                {#if incidentDetections(inc).length > 8}
                  <div class="det-overflow">
                    +{incidentDetections(inc).length - 8} more detections
                  </div>
                {/if}
              </div>
            {/if}
          </div>

          <!-- Chevron indicator -->
          <div class="chevron" class:open={isOpen}>›</div>
        </article>
      {/each}
    </div>
  {/if}
</div>

<style>
  /* ── View root ────────────────────────────────────────────────────────────── */
  .view { min-width: 0; overflow-x: hidden; }

  /* ── Header ──────────────────────────────────────────────────────────────── */
  .open-count {
    font-size: var(--text-small);
    color: var(--text-secondary);
    margin-bottom: 4px;
    align-self: flex-end;
  }

  /* ── Skeletons ───────────────────────────────────────────────────────────── */
  .skeleton-stack { display: grid; gap: 6px; }
  .inc-skeleton {
    height: 68px;
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    animation: pulse var(--duration-medium) ease-in-out infinite;
  }
  @keyframes pulse { 0%, 100% { opacity: 0.5; } 50% { opacity: 0.2; } }

  /* ── Cards ───────────────────────────────────────────────────────────────── */
  .incident-list { display: grid; gap: 5px; }

  .inc-card {
    display: flex;
    align-items: stretch;
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    overflow: hidden;
    cursor: pointer;
    transition:
      border-color var(--duration-instant) var(--ease-out),
      background   var(--duration-instant) var(--ease-out);
  }

  .inc-card:hover {
    border-color: var(--border-default);
    background: var(--bg-elevated);
  }

  .inc-card:focus-visible {
    outline: 2px solid var(--accent-primary);
    outline-offset: 2px;
  }

  /* ── Severity stripe ─────────────────────────────────────────────────────── */
  .sev-stripe {
    width: 3px;
    flex-shrink: 0;
    background: var(--state-neutral);
    transition: background var(--duration-instant) var(--ease-out);
  }
  .sev-critical .sev-stripe { background: var(--state-failed); }
  .sev-warn     .sev-stripe { background: var(--state-degraded); }
  .sev-info     .sev-stripe { background: var(--state-info); }

  /* ── Card body ───────────────────────────────────────────────────────────── */
  .inc-body {
    flex: 1;
    padding: 10px var(--card-pad);
    min-width: 0;
    overflow: hidden;
  }

  .row-primary {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    min-width: 0;
  }

  .device-addr {
    font-family: var(--font-mono);
    font-size: var(--text-mono);
    font-weight: 600;
    color: var(--text-primary);
    letter-spacing: var(--tracking-mono);
    max-width: 240px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex-shrink: 1;
  }

  .rule-pills {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
    min-width: 0;
    flex-shrink: 1;
  }

  .rule-pill {
    font-size: 11px;
    font-family: var(--font-mono);
    padding: 1px 6px;
    border-radius: 3px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    color: var(--text-secondary);
    white-space: nowrap;
    letter-spacing: 0.01em;
  }

  .spacer { flex: 1; min-width: 8px; }

  .ev-count {
    font-size: var(--text-small);
    color: var(--text-tertiary);
    white-space: nowrap;
  }

  .inc-age {
    font-size: var(--text-small);
    color: var(--text-tertiary);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  /* ── Secondary row ───────────────────────────────────────────────────────── */
  .row-secondary {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 4px;
    flex-wrap: wrap;
  }

  .context-line {
    font-size: var(--text-small);
    color: var(--text-secondary);
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tag {
    font-size: 11px;
    padding: 1px 6px;
    border-radius: 3px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    color: var(--text-tertiary);
    white-space: nowrap;
    letter-spacing: 0.01em;
  }

  .rem-succeeded { color: var(--state-healthy);  border-color: rgba(52,211,153,0.2); }
  .rem-failed    { color: var(--state-failed);   border-color: rgba(248,113,113,0.2); }
  .rem-pending   { color: var(--state-degraded); border-color: rgba(251,191,36,0.2); }

  /* ── Expanded detection list ─────────────────────────────────────────────── */
  .expanded-body {
    margin-top: 10px;
    padding-top: 10px;
    border-top: 1px solid var(--border-subtle);
  }

  .det-row {
    display: grid;
    grid-template-columns: 72px minmax(0,1fr) minmax(0,1fr) auto auto auto;
    gap: 8px;
    align-items: center;
    min-height: var(--row-height);
    padding: 3px 4px;
    border-radius: 3px;
    background: transparent;
    border: none;
    color: var(--text-secondary);
    text-align: left;
    font-size: var(--text-small);
    font-family: inherit;
    width: 100%;
    cursor: pointer;
    transition: background var(--duration-instant) var(--ease-out);
    overflow: hidden;
  }

  .det-row:hover:not(:disabled) {
    background: var(--bg-glass);
    color: var(--text-primary);
  }

  .det-row:disabled { cursor: default; opacity: 0.55; }

  .det-ts {
    font-family: var(--font-mono);
    font-size: var(--text-mono-sm);
    color: var(--text-tertiary);
    font-variant-numeric: tabular-nums;
  }

  .det-device {
    font-family: var(--font-mono);
    font-size: var(--text-mono-sm);
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }

  .det-rule {
    font-size: var(--text-small);
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .det-trace {
    font-size: 11px;
    color: var(--accent-primary);
    white-space: nowrap;
  }
  .det-sources { display: inline-flex; gap: 3px; flex-wrap: wrap; }
  .src-pill {
    display: inline-block;
    background: rgba(255,255,255,0.06);
    border: 1px solid var(--border, rgba(255,255,255,0.12));
    color: var(--text-secondary, #9ca3af);
    font-size: 10px;
    padding: 1px 5px;
    border-radius: 8px;
  }
  .det-latency {
    font-size: 10px;
    color: var(--text-tertiary, #6b7280);
    white-space: nowrap;
  }

  .det-overflow {
    padding: 4px 4px;
    font-size: 11px;
    color: var(--text-tertiary);
  }

  /* ── Provenance / correlation chain ──────────────────────────────────────── */
  .provenance-section {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 6px 0 4px;
    border-bottom: 1px solid var(--border-subtle);
    margin-bottom: 6px;
  }
  .provenance-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-tertiary);
    margin-right: 4px;
  }
  .chain-step {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: rgba(255,255,255,0.04);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    padding: 2px 7px;
    font-size: 11px;
  }
  .chain-src {
    font-size: 10px;
    font-weight: 600;
    padding: 1px 4px;
    border-radius: 4px;
    background: rgba(255,255,255,0.06);
  }
  .chain-src.src-gnmi    { color: #4dd0c8; }
  .chain-src.src-syslog  { color: #a78bfa; }
  .chain-src.src-snmp    { color: #f59e0b; }
  .chain-src.src-netflow { color: #3b82f6; }
  .chain-src.src-otlp    { color: #10b981; }
  .chain-src.src-bmp     { color: #f97316; }
  .chain-src.src-bgp_ls  { color: #ec4899; }
  .chain-type { color: var(--text-secondary); }
  .chain-dev  { font-size: 10px; color: var(--text-tertiary); }

  /* ── Blast radius summary ─────────────────────────────────────────────────── */
  .blast-summary {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    padding: 4px 0 6px;
  }
  .blast-chip {
    font-size: 11px;
    background: rgba(239,68,68,0.08);
    border: 1px solid rgba(239,68,68,0.25);
    color: #fca5a5;
    border-radius: 6px;
    padding: 2px 8px;
  }

  /* ── Chevron ─────────────────────────────────────────────────────────────── */
  .chevron {
    display: flex;
    align-items: center;
    padding: 0 12px;
    color: var(--text-tertiary);
    font-size: 18px;
    line-height: 1;
    user-select: none;
    transition: transform var(--duration-fast) var(--ease-out);
    flex-shrink: 0;
  }
  .chevron.open { transform: rotate(90deg); }

  /* ── Config correlation hint (D2-4 T5) ──────────────────────────────────── */
  .config-hint {
    margin-top: 6px;
    padding: 5px 10px;
    background: rgba(77,208,200,0.08);
    border-left: 2px solid #4dd0c8;
    border-radius: 3px;
    font-size: var(--text-small);
    color: #4dd0c8;
  }
</style>
