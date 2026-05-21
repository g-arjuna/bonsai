<script>
  import { onMount } from 'svelte';

  let { id } = $props();

  let steps = $state([]);
  let loading = $state(true);
  let error = $state(null);

  const SRC_COLOR = {
    gnmi:    '#4dd0c8',
    syslog:  '#a78bfa',
    snmp:    '#f59e0b',
    netflow: '#3b82f6',
    otlp:    '#10b981',
    bmp:     '#f97316',
    bgp_ls:  '#ec4899',
    detection:   '#f85149',
    remediation: '#3fb950',
  };

  const KIND_LABEL = {
    trigger:     'Signal',
    detection:   'Detection',
    remediation: 'Remediation',
  };

  function fmt(ns) {
    if (!ns) return '—';
    return new Date(ns / 1e6).toISOString().replace('T', ' ').slice(0, 19);
  }
  function shortFmt(ns) {
    if (!ns) return '—';
    return new Date(ns / 1e6).toISOString().slice(11, 23);
  }

  let triggerSteps = $derived(steps.filter(s => s.kind === 'trigger'));
  let detectionStep = $derived(steps.find(s => s.kind === 'detection'));
  let remediationSteps = $derived(steps.filter(s => s.kind === 'remediation'));

  let windowMin = $derived(triggerSteps.length ? Math.min(...triggerSteps.map(s => s.occurred_at_ns)) : 0);
  let windowMax = $derived(triggerSteps.length ? Math.max(...triggerSteps.map(s => s.occurred_at_ns)) : 0);
  let windowSpanMs = $derived(windowMax > windowMin ? (windowMax - windowMin) / 1e6 : 0);

  const CORR_WINDOW_MS = 45_000;

  function posPct(ns) {
    if (!triggerSteps.length) return 0;
    const span = Math.max(windowSpanMs, 1000);
    return Math.min(100, Math.max(0, ((ns - windowMin) / 1e6) / span * 100));
  }

  function parseDetail(step) {
    if (!step?.detail_json) return null;
    try { return JSON.parse(step.detail_json); } catch { return null; }
  }

  async function load() {
    if (!id) { loading = false; return; }
    try {
      const r = await fetch(`/api/trace/${encodeURIComponent(id)}`);
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      steps = data.steps ?? [];
      error = null;
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  onMount(load);
</script>

<div class="trace-root">
  <!-- Header -->
  <div class="trace-header">
    <div class="trace-title">
      <span class="trace-eyebrow">Closed-Loop Trace</span>
      <h2 class="trace-heading">
        {detectionStep?.rule_id || 'Detection'}
        {#if detectionStep?.device_address}
          <span class="trace-device">{detectionStep.device_address}</span>
        {/if}
      </h2>
    </div>
    {#if id}
      <code class="trace-id">{id.slice(0, 12)}…</code>
    {/if}
  </div>

  {#if !id}
    <p class="empty-msg">No detection selected. Click "Trace &amp; Explain" from an incident.</p>
  {:else if loading}
    <div class="trace-skeleton-stack">
      {#each [1,2,3] as _}<div class="trace-skeleton"></div>{/each}
    </div>
  {:else if error}
    <div class="notice error">{error}</div>
  {:else if !steps.length}
    <p class="empty-msg">No trace steps found for detection <code>{id}</code>.</p>
  {:else}

    <!-- ── 45-second correlation window visualization ── -->
    {#if triggerSteps.length > 0}
      <section class="section">
        <div class="section-label">
          45s Correlation Window
          <span class="section-hint">
            {triggerSteps.length} signal{triggerSteps.length !== 1 ? 's' : ''} ·
            {windowSpanMs < 1 ? '<1' : Math.round(windowSpanMs)}ms span
          </span>
        </div>
        <div class="corr-window">
          <div class="corr-track">
            <!-- Window fill bar (proportional to CORR_WINDOW_MS) -->
            <div
              class="corr-fill"
              style="width:{Math.min(100, windowSpanMs / CORR_WINDOW_MS * 100).toFixed(1)}%"
            ></div>
            <!-- Event markers -->
            {#each triggerSteps as step}
              {@const pct = posPct(step.occurred_at_ns)}
              <div
                class="corr-marker"
                style="left:{pct}%; background:{SRC_COLOR[step.source_type] ?? '#6b7280'};"
                title="{step.source_type} — {step.event_type} @ {shortFmt(step.occurred_at_ns)}"
              ></div>
            {/each}
          </div>
          <div class="corr-legend">
            {#each triggerSteps as step, i}
              <span class="corr-leg-item" style="color:{SRC_COLOR[step.source_type] ?? '#6b7280'}">
                {step.source_type}
              </span>
            {/each}
          </div>
        </div>
      </section>
    {/if}

    <!-- ── Source signals (trigger steps) ── -->
    {#if triggerSteps.length > 0}
      <section class="section">
        <div class="section-label">Contributing Signals</div>
        <div class="timeline">
          {#each triggerSteps as step, i}
            {@const detail = parseDetail(step)}
            <div class="tl-row">
              <div class="tl-time">{shortFmt(step.occurred_at_ns)}</div>
              <div class="tl-connector">
                <div class="tl-dot" style="background:{SRC_COLOR[step.source_type] ?? '#6b7280'}"></div>
                {#if i < triggerSteps.length - 1}<div class="tl-line"></div>{/if}
              </div>
              <div class="tl-body">
                <div class="tl-row-top">
                  <span class="src-badge" style="color:{SRC_COLOR[step.source_type] ?? '#6b7280'}">{step.source_type}</span>
                  <span class="tl-etype">{step.event_type}</span>
                  {#if step.device_address}
                    <code class="tl-dev">{step.device_address}</code>
                  {/if}
                </div>
                {#if detail}
                  <div class="tl-detail">{JSON.stringify(detail, null, 2)}</div>
                {/if}
              </div>
            </div>
          {/each}
        </div>
      </section>
    {/if}

    <!-- ── Detection ── -->
    {#if detectionStep}
      <section class="section">
        <div class="section-label">Detection Fired</div>
        <div class="det-card">
          <div class="det-card-row">
            <span class="sev-badge sev-{detectionStep.severity?.toLowerCase()}">{detectionStep.severity ?? 'unknown'}</span>
            <code class="det-rule">{detectionStep.rule_id}</code>
            <code class="det-dev">{detectionStep.device_address}</code>
            <time class="det-ts">{fmt(detectionStep.occurred_at_ns)}</time>
          </div>
          {#if detectionStep.action}
            <div class="det-action">Action: <code>{detectionStep.action}</code></div>
          {/if}
        </div>
      </section>
    {/if}

    <!-- ── Remediation history (HITL) ── -->
    {#if remediationSteps.length > 0}
      <section class="section">
        <div class="section-label">Remediation / HITL History</div>
        {#each remediationSteps as step}
          <div class="rem-row">
            <span class="rem-status rem-{step.status?.toLowerCase()}">{step.status ?? 'unknown'}</span>
            <code class="rem-action">{step.action}</code>
            <code class="rem-dev">{step.device_address}</code>
            <time class="rem-ts">{fmt(step.occurred_at_ns)}</time>
          </div>
        {/each}
      </section>
    {/if}

    <!-- ── Full step table ── -->
    <details class="raw-steps">
      <summary>Raw trace steps ({steps.length})</summary>
      <div class="raw-table">
        {#each steps as step}
          <div class="raw-row">
            <span class="raw-kind raw-kind-{step.kind}">{KIND_LABEL[step.kind] ?? step.kind}</span>
            <time class="raw-ts">{fmt(step.occurred_at_ns)}</time>
            <code class="raw-dev">{step.device_address || '—'}</code>
            <span class="raw-type">{step.event_type || step.rule_id || ''}</span>
            {#if step.status}<span class="raw-status">{step.status}</span>{/if}
          </div>
        {/each}
      </div>
    </details>

  {/if}
</div>

<style>
  .trace-root {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  /* ── Header ──────────────────────────────────────────────── */
  .trace-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }
  .trace-eyebrow {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-tertiary);
  }
  .trace-heading {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex-wrap: wrap;
  }
  .trace-device {
    font-family: var(--font-mono);
    font-size: 13px;
    font-weight: 400;
    color: var(--text-secondary);
  }
  .trace-id {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-tertiary);
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    padding: 2px 6px;
    white-space: nowrap;
  }

  /* ── Sections ─────────────────────────────────────────────── */
  .section {
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    padding: 10px 14px;
  }
  .section-label {
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-tertiary);
    margin-bottom: 8px;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .section-hint {
    font-weight: 400;
    text-transform: none;
    letter-spacing: 0;
    color: var(--text-tertiary);
  }

  /* ── 45s correlation window ───────────────────────────────── */
  .corr-window { display: flex; flex-direction: column; gap: 8px; }
  .corr-track {
    position: relative;
    height: 24px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    overflow: hidden;
  }
  .corr-fill {
    position: absolute;
    left: 0; top: 0; height: 100%;
    background: rgba(88,166,255,0.08);
    border-right: 1px dashed rgba(88,166,255,0.3);
    transition: width 0.3s;
  }
  .corr-marker {
    position: absolute;
    top: 4px;
    width: 3px;
    height: 16px;
    border-radius: 2px;
    transform: translateX(-50%);
    cursor: default;
  }
  .corr-legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .corr-leg-item {
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.03em;
    background: rgba(255,255,255,0.05);
    border: 1px solid var(--border-subtle);
    padding: 1px 6px;
    border-radius: 4px;
  }

  /* ── Timeline ─────────────────────────────────────────────── */
  .timeline { display: flex; flex-direction: column; gap: 0; }
  .tl-row {
    display: grid;
    grid-template-columns: 84px 20px 1fr;
    gap: 8px;
    align-items: start;
  }
  .tl-time {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-tertiary);
    padding-top: 3px;
    text-align: right;
  }
  .tl-connector {
    display: flex;
    flex-direction: column;
    align-items: center;
    padding-top: 4px;
  }
  .tl-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .tl-line {
    width: 2px;
    flex: 1;
    min-height: 12px;
    background: var(--border-subtle);
  }
  .tl-body {
    padding: 2px 0 10px;
  }
  .tl-row-top {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 2px;
  }
  .src-badge {
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    padding: 1px 5px;
    background: rgba(255,255,255,0.05);
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
  }
  .tl-etype {
    font-size: var(--text-small);
    color: var(--text-secondary);
  }
  .tl-dev {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-tertiary);
  }
  .tl-detail {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-tertiary);
    white-space: pre-wrap;
    word-break: break-all;
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    padding: 4px 8px;
    margin-top: 4px;
    max-height: 80px;
    overflow: auto;
  }

  /* ── Detection card ───────────────────────────────────────── */
  .det-card {
    background: rgba(248,81,73,0.06);
    border: 1px solid rgba(248,81,73,0.2);
    border-radius: 5px;
    padding: 8px 12px;
  }
  .det-card-row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
  }
  .sev-badge {
    font-size: 10px;
    font-weight: 700;
    text-transform: uppercase;
    padding: 1px 6px;
    border-radius: 3px;
    letter-spacing: 0.04em;
  }
  .sev-critical { background: rgba(239,68,68,0.2);  color: #fca5a5; }
  .sev-high     { background: rgba(251,146,60,0.2); color: #fdba74; }
  .sev-warn, .sev-warning { background: rgba(251,191,36,0.2); color: #fde68a; }
  .sev-info     { background: rgba(59,130,246,0.2); color: #93c5fd; }
  .det-rule { font-size: 12px; color: var(--text-primary); }
  .det-dev  { font-size: 11px; color: var(--text-secondary); }
  .det-ts   { font-size: 11px; color: var(--text-tertiary); margin-left: auto; }
  .det-action { margin-top: 4px; font-size: 11px; color: var(--text-secondary); }

  /* ── Remediation ──────────────────────────────────────────── */
  .rem-row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    padding: 4px 0;
    border-bottom: 1px solid var(--border-subtle);
    font-size: var(--text-small);
  }
  .rem-row:last-child { border-bottom: none; }
  .rem-status {
    font-size: 10px;
    font-weight: 700;
    text-transform: uppercase;
    padding: 1px 6px;
    border-radius: 3px;
    letter-spacing: 0.04em;
  }
  .rem-succeeded { background: rgba(52,211,153,0.15); color: #6ee7b7; }
  .rem-pending   { background: rgba(251,191,36,0.15); color: #fde68a; }
  .rem-failed    { background: rgba(248,81,73,0.15);  color: #fca5a5; }
  .rem-approved  { background: rgba(52,211,153,0.15); color: #6ee7b7; }
  .rem-rejected  { background: rgba(248,81,73,0.15);  color: #fca5a5; }
  .rem-action { font-size: 12px; color: var(--text-primary); }
  .rem-dev    { font-size: 11px; color: var(--text-secondary); }
  .rem-ts     { font-size: 11px; color: var(--text-tertiary); margin-left: auto; }

  /* ── Raw steps collapsible ────────────────────────────────── */
  .raw-steps {
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    overflow: hidden;
  }
  .raw-steps summary {
    padding: 8px 14px;
    font-size: 11px;
    color: var(--text-tertiary);
    cursor: pointer;
    user-select: none;
  }
  .raw-steps summary:hover { color: var(--text-secondary); }
  .raw-table { padding: 0 14px 10px; display: flex; flex-direction: column; gap: 2px; }
  .raw-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 11px;
    padding: 2px 0;
    border-bottom: 1px solid var(--border-subtle);
  }
  .raw-row:last-child { border-bottom: none; }
  .raw-kind {
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 1px 5px;
    border-radius: 3px;
    white-space: nowrap;
  }
  .raw-kind-trigger     { background: rgba(88,166,255,0.12); color: #58a6ff; }
  .raw-kind-detection   { background: rgba(248,81,73,0.12);  color: #f85149; }
  .raw-kind-remediation { background: rgba(63,185,80,0.12);  color: #3fb950; }
  .raw-ts   { font-family: var(--font-mono); font-size: 10px; color: var(--text-tertiary); white-space: nowrap; }
  .raw-dev  { font-family: var(--font-mono); font-size: 10px; color: var(--text-secondary); }
  .raw-type { flex: 1; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .raw-status { font-size: 10px; color: var(--text-tertiary); white-space: nowrap; }

  /* ── Misc ─────────────────────────────────────────────────── */
  .empty-msg { color: var(--text-secondary); font-size: var(--text-small); }
  .trace-skeleton-stack { display: grid; gap: 8px; }
  .trace-skeleton {
    height: 64px;
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    animation: pulse 1.5s ease-in-out infinite;
  }
  @keyframes pulse { 0%, 100% { opacity: 0.5; } 50% { opacity: 0.2; } }
</style>
