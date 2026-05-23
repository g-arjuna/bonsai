<script>
  import { onMount, onDestroy } from 'svelte';

  let data = $state(null);
  let loading = $state(true);
  let error = $state('');
  let interval;
  let activeTab = $state('status');

  // D4-9 T4: Rules visibility
  let rulesData = $state(null);
  let rulesLoading = $state(false);
  let rulesFilter = $state('');
  let togglingRule = $state('');

  onMount(() => {
    loadStatus();
    interval = setInterval(loadStatus, 8000);
  });
  onDestroy(() => clearInterval(interval));

  async function loadRules() {
    rulesLoading = true;
    try {
      const r = await fetch('/api/sidecar/rules');
      if (!r.ok) throw new Error(await r.text());
      rulesData = await r.json();
    } catch (e) {
      error = e.message;
    } finally {
      rulesLoading = false;
    }
  }

  async function toggleRule(ruleId) {
    togglingRule = ruleId;
    try {
      const r = await fetch(`/api/sidecar/rules/${encodeURIComponent(ruleId)}/toggle`, { method: 'POST' });
      if (!r.ok) throw new Error(await r.text());
      await loadRules();
    } catch (e) {
      error = e.message;
    } finally {
      togglingRule = '';
    }
  }

  function switchTab(tab) {
    activeTab = tab;
    if (tab === 'rules' && !rulesData) loadRules();
  }

  let filteredRules = $derived(
    rulesData?.rules
      ? (rulesFilter.trim()
          ? rulesData.rules.filter(r => r.rule_id.includes(rulesFilter) || r.sidecar_name.includes(rulesFilter))
          : rulesData.rules)
      : []
  );

  async function loadStatus() {
    try {
      const r = await fetch('/api/sidecar/status');
      if (!r.ok) throw new Error(await r.text());
      data = await r.json();
      error = '';
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function fmtUptime(secs) {
    if (!secs) return '—';
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    if (h > 0) return `${h}h ${m}m`;
    return `${m}m`;
  }

  function relativeNs(ns) {
    if (!ns) return '—';
    const ms = (Date.now() - ns / 1e6);
    if (ms < 0) return 'just now';
    if (ms < 60000) return `${Math.round(ms / 1000)}s ago`;
    if (ms < 3600000) return `${Math.round(ms / 60000)}m ago`;
    return `${Math.round(ms / 3600000)}h ago`;
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">ML / Detection Engine</p>
      <h2>Sidecars</h2>
    </div>
    <button class="ghost" onclick={() => activeTab === 'rules' ? loadRules() : loadStatus()}>Refresh</button>
  </div>

  <div class="tabs">
    <button class="tab" class:active={activeTab === 'status'} onclick={() => switchTab('status')}>Status</button>
    <button class="tab" class:active={activeTab === 'rules'} onclick={() => switchTab('rules')}>Rules</button>
  </div>

  {#if activeTab === 'rules'}
    {#if rulesLoading}
      <p class="muted">Loading rules…</p>
    {:else if !rulesData?.rules?.length}
      <div class="empty-state">
        <p>No sidecar rules found.</p>
        <p class="muted small">Rules appear once a sidecar registers with its capabilities list.</p>
      </div>
    {:else}
      <div class="rules-toolbar">
        <input bind:value={rulesFilter} placeholder="Filter by rule ID or sidecar…" class="rules-filter" />
        <span class="muted" style="font-size:12px;">{filteredRules.length} / {rulesData.rules.length} rules</span>
      </div>
      <div class="rules-list">
        {#each filteredRules as rule}
          <div class="rule-row" class:disabled={!rule.enabled}>
            <div class="rule-info">
              <span class="rule-id">{rule.rule_id}</span>
              <span class="rule-sidecar muted">{rule.sidecar_name} · {rule.sidecar_kind}</span>
            </div>
            <button
              class="toggle-btn"
              class:on={rule.enabled}
              disabled={togglingRule === rule.rule_id}
              onclick={() => toggleRule(rule.rule_id)}
              title={rule.enabled ? 'Disable rule' : 'Enable rule'}
            >
              {rule.enabled ? '● Enabled' : '○ Disabled'}
            </button>
          </div>
        {/each}
      </div>
    {/if}

  {:else if loading}
    <p class="muted">Loading sidecar status…</p>
  {:else if error}
    <p class="error-msg">{error}</p>
  {:else if !data?.sidecars?.length}
    <div class="empty-state">
      <p>No sidecars registered.</p>
      <p class="muted small">Start the Python collector engine (<code>python3 python/collector_engine.py</code>) or GNN sidecar to see entries here.</p>
    </div>
  {:else}
    <div class="sidecar-grid">
      {#each data.sidecars as sc}
        {@const ok = sc.health_reachable && sc.status === 'healthy'}
        <div class="sidecar-card" class:healthy={ok} class:degraded={!sc.health_reachable && sc.status === 'healthy'} class:unhealthy={sc.status !== 'healthy'}>
          <div class="sc-header">
            <span class="sc-name">{sc.name}</span>
            <span class="sc-kind">{sc.kind}</span>
            <span class="sc-status status-{sc.status}">{sc.status}</span>
          </div>

          <div class="sc-stats">
            <div class="stat">
              <span class="stat-label">Rules Loaded</span>
              <span class="stat-val">{sc.rules_loaded}</span>
            </div>
            <div class="stat">
              <span class="stat-label">Detections Today</span>
              <span class="stat-val">{sc.detections_today}</span>
            </div>
            <div class="stat">
              <span class="stat-label">Queue Depth</span>
              <span class="stat-val" class:queue-warn={sc.queue_depth > 100}>{sc.queue_depth}</span>
            </div>
            <div class="stat">
              <span class="stat-label">Uptime</span>
              <span class="stat-val">{fmtUptime(sc.uptime_secs)}</span>
            </div>
          </div>

          <div class="sc-footer">
            <span class="sc-ver">{sc.version ?? 'v?'}</span>
            <span class="sc-last">Last det: {relativeNs(sc.last_detection_at_ns)}</span>
            {#if !sc.health_reachable}
              <span class="health-warn">⚠ health endpoint unreachable</span>
            {/if}
          </div>
        </div>
      {/each}
    </div>

    <div class="info-panel">
      <h3>Configuration</h3>
      <p class="muted small">
        Set <code>BONSAI_REQUIRE_SIDECAR=collector-engine</code> to make the health check degrade until a sidecar of that kind registers.
        The Python collector engine registers automatically on startup via gRPC.
      </p>
      <p class="muted small" style="margin-top:6px">
        Health endpoint: <code>http://&lt;sidecar-host&gt;:9200/health</code> — override port with <code>BONSAI_SIDECAR_HEALTH_PORT</code>.
      </p>
    </div>
  {/if}
</div>

<style>
  .sidecar-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 12px; margin-bottom: 20px; }

  .sidecar-card {
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    padding: 14px 16px;
    border-left: 3px solid var(--state-neutral, #6b7280);
  }
  .sidecar-card.healthy { border-left-color: var(--state-healthy, #22c55e); }
  .sidecar-card.degraded { border-left-color: var(--state-degraded, #f59e0b); }
  .sidecar-card.unhealthy { border-left-color: var(--state-failed, #ef4444); }

  .sc-header { display: flex; align-items: center; gap: 8px; margin-bottom: 10px; }
  .sc-name { font-weight: 600; font-size: 13px; color: var(--text-primary); flex: 1; }
  .sc-kind { font-size: 10px; text-transform: uppercase; letter-spacing: 0.04em; color: var(--text-tertiary); background: var(--bg-elevated); padding: 1px 6px; border-radius: 3px; }
  .sc-status { font-size: 10px; font-weight: 700; text-transform: uppercase; padding: 1px 7px; border-radius: 10px; }
  .status-healthy { background: rgba(34,197,94,0.12); color: #22c55e; }
  .status-degraded { background: rgba(245,158,11,0.12); color: #f59e0b; }
  .status-unhealthy, .status-missing { background: rgba(239,68,68,0.12); color: #ef4444; }

  .sc-stats { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; margin-bottom: 10px; }
  .stat { background: var(--bg-elevated); border-radius: 4px; padding: 6px 10px; }
  .stat-label { display: block; font-size: 10px; text-transform: uppercase; letter-spacing: 0.04em; color: var(--text-tertiary); margin-bottom: 2px; }
  .stat-val { font-size: 18px; font-weight: 700; color: var(--text-primary); font-variant-numeric: tabular-nums; }
  .queue-warn { color: var(--state-degraded, #f59e0b); }

  .sc-footer { display: flex; align-items: center; gap: 10px; font-size: 11px; color: var(--text-tertiary); flex-wrap: wrap; }
  .sc-ver { font-family: var(--font-mono); }
  .health-warn { color: #f59e0b; }

  .info-panel { background: var(--bg-surface); border: 1px solid var(--border-subtle); border-radius: 6px; padding: 14px 16px; }
  .info-panel h3 { margin: 0 0 8px; font-size: 13px; font-weight: 600; }

  .empty-state { padding: 32px; text-align: center; color: var(--text-secondary); }
  .muted { color: var(--text-tertiary); }
  .small { font-size: 11px; }
  .error-msg { color: #fca5a5; font-size: 12px; }

  button.ghost { padding: 5px 12px; background: none; border: 1px solid var(--border-subtle); border-radius: 4px; color: var(--text-secondary); cursor: pointer; font-size: 12px; }
  button.ghost:hover { border-color: var(--border-default); color: var(--text-primary); }

  /* Tabs */
  .tabs { display: flex; gap: 4px; margin-bottom: 16px; border-bottom: 1px solid var(--border-subtle); padding-bottom: 8px; }
  .tab { background: transparent; border: none; color: var(--text-tertiary); font-size: 13px; cursor: pointer; padding: 6px 12px; border-radius: 4px; }
  .tab:hover { background: rgba(255,255,255,0.05); }
  .tab.active { color: var(--text-primary); font-weight: 600; background: rgba(255,255,255,0.08); }

  /* Rules tab */
  .rules-toolbar { display: flex; align-items: center; gap: 10px; margin-bottom: 12px; }
  .rules-filter { flex: 1; max-width: 340px; padding: 6px 10px; font-size: 12px; border: 1px solid var(--border-subtle); border-radius: 4px; background: var(--bg-elevated); color: var(--text-primary); }
  .rules-list { display: flex; flex-direction: column; gap: 4px; }
  .rule-row {
    display: flex; align-items: center; justify-content: space-between;
    padding: 8px 12px;
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    transition: opacity 0.15s;
  }
  .rule-row.disabled { opacity: 0.5; }
  .rule-info { display: flex; flex-direction: column; gap: 2px; }
  .rule-id { font-size: 13px; font-weight: 600; font-family: var(--font-mono); color: var(--text-primary); }
  .rule-sidecar { font-size: 11px; }
  .toggle-btn { background: none; border: 1px solid var(--border-subtle); border-radius: 4px; padding: 4px 10px; cursor: pointer; font-size: 11px; color: var(--text-tertiary); }
  .toggle-btn.on { color: var(--state-healthy, #22c55e); border-color: rgba(34,197,94,0.3); }
  .toggle-btn:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
