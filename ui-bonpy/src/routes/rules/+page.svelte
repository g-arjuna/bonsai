<script>
  import { onMount } from 'svelte';
  import { api } from '$lib/api.js';

  // ── state ─────────────────────────────────────────────────────────────────
  let tab = 'detection';   // 'detection' | 'playbooks' | 'syslog'

  // Detection rules
  let rules = [];
  let analytics = {};      // rule_id → {firing_count, last_fired_ns, severity}
  let rulesLoading = true;
  let rulesError = null;

  // Per-rule panel state
  let expandedRule = null;
  let paramsEditing = {};   // rule_id → params object being edited
  let shadowFirings = {};   // rule_id → [{fired_at_ns, device_address, reason}]

  // Playbooks
  let playbooks = [];
  let playbookStats = {};
  let playbooksLoading = true;
  let playbooksError = null;
  let showNewPlaybook = false;
  let newPb = { name: '', description: '', steps_json: '[]', enabled: true };
  let pbSaving = false;

  // Syslog rules
  let syslogRules = [];
  let syslogLoading = true;
  let showNewSyslog = false;
  let newSyslog = { rule_id: '', description: '', pattern: '', event_type: 'syslog_match', severity: 'warn', vendor: '', shadow_mode: false };
  let syslogSaving = false;

  // ── lifecycle ─────────────────────────────────────────────────────────────
  onMount(() => {
    loadRules();
    loadPlaybooks();
    loadSyslogRules();
  });

  async function loadRules() {
    rulesLoading = true; rulesError = null;
    try {
      const [rData, aData] = await Promise.all([api.rules.list(), api.rules.analytics()]);
      rules = rData?.rules ?? [];
      const aRows = aData?.analytics ?? [];
      analytics = {};
      for (const r of aRows) analytics[r.rule_id] = r;
    } catch (e) { rulesError = e.message; }
    finally { rulesLoading = false; }
  }

  async function loadPlaybooks() {
    playbooksLoading = true; playbooksError = null;
    try {
      const [pbData, stData] = await Promise.all([api.playbooks.list(), api.playbooks.stats()]);
      playbooks = pbData?.playbooks ?? [];
      const statsArr = stData?.stats ?? [];
      playbookStats = {};
      for (const s of statsArr) playbookStats[s.id] = s;
    } catch (e) { playbooksError = e.message; }
    finally { playbooksLoading = false; }
  }

  async function loadSyslogRules() {
    syslogLoading = true;
    try {
      const d = await api.syslogRules.list();
      syslogRules = d?.syslog_rules ?? [];
    } catch (_) {}
    finally { syslogLoading = false; }
  }

  // ── rule actions ──────────────────────────────────────────────────────────
  async function toggleRule(rule) {
    await api.rules.toggle(rule.rule_id);
    rule.enabled = !rule.enabled;
    rules = rules;
  }

  async function toggleShadow(rule) {
    const newVal = !rule.shadow_mode;
    await api.rules.setShadow(rule.rule_id, newVal);
    rule.shadow_mode = newVal;
    rules = rules;
  }

  async function expandRule(rule) {
    if (expandedRule === rule.rule_id) { expandedRule = null; return; }
    expandedRule = rule.rule_id;
    if (!paramsEditing[rule.rule_id]) {
      try {
        const p = await api.rules.getParams(rule.rule_id);
        paramsEditing[rule.rule_id] = JSON.stringify(p?.parameters ?? {}, null, 2);
      } catch (_) { paramsEditing[rule.rule_id] = '{}'; }
    }
    if (rule.shadow_mode && !shadowFirings[rule.rule_id]) {
      try {
        const sf = await api.rules.shadowFirings(rule.rule_id);
        shadowFirings[rule.rule_id] = sf?.shadow_firings ?? [];
      } catch (_) { shadowFirings[rule.rule_id] = []; }
    }
  }

  async function saveParams(rule) {
    try {
      const params = JSON.parse(paramsEditing[rule.rule_id] || '{}');
      await api.rules.patchParams(rule.rule_id, params);
    } catch (e) { alert('Invalid JSON: ' + e.message); }
  }

  // ── playbook actions ──────────────────────────────────────────────────────
  async function saveNewPlaybook() {
    pbSaving = true;
    try {
      await api.playbooks.create(newPb);
      newPb = { name: '', description: '', steps_json: '[]', enabled: true };
      showNewPlaybook = false;
      await loadPlaybooks();
    } catch (e) { alert(e.message); }
    finally { pbSaving = false; }
  }

  async function deletePlaybook(pb) {
    if (!confirm(`Delete playbook "${pb.name}"?`)) return;
    await api.playbooks.del(pb.id);
    await loadPlaybooks();
  }

  // ── syslog rule actions ───────────────────────────────────────────────────
  async function saveNewSyslogRule() {
    if (!newSyslog.rule_id || !newSyslog.pattern) { alert('rule_id and pattern are required'); return; }
    syslogSaving = true;
    try {
      await api.syslogRules.create(newSyslog);
      newSyslog = { rule_id: '', description: '', pattern: '', event_type: 'syslog_match', severity: 'warn', vendor: '', shadow_mode: false };
      showNewSyslog = false;
      await loadSyslogRules();
    } catch (e) { alert(e.message); }
    finally { syslogSaving = false; }
  }

  // ── helpers ───────────────────────────────────────────────────────────────
  function fmtTs(ns) {
    if (!ns) return '—';
    return new Date(Number(ns) / 1e6).toLocaleString();
  }

  function severityClass(sev) {
    return { critical: 'sev-crit', high: 'sev-high', warn: 'sev-warn', info: 'sev-info' }[sev] ?? 'sev-info';
  }
</script>

<div class="rules-page">
  <header class="page-hdr">
    <h1>Rules &amp; Playbooks</h1>
    <div class="tab-bar">
      <button class="tab-btn" class:active={tab==='detection'} on:click={() => tab='detection'}>Detection Rules</button>
      <button class="tab-btn" class:active={tab==='playbooks'} on:click={() => tab='playbooks'}>Playbooks</button>
      <button class="tab-btn" class:active={tab==='syslog'}    on:click={() => tab='syslog'}>Syslog Patterns</button>
    </div>
  </header>

  <!-- ── Detection Rules Tab ────────────────────────────────────────────── -->
  {#if tab === 'detection'}
    <section class="tab-content">
      <div class="section-hdr">
        <span class="section-title">Registered Rules <span class="badge">{rules.length}</span></span>
        <button class="btn-ghost" on:click={loadRules}>↻ Refresh</button>
      </div>

      {#if rulesLoading}
        <div class="loading">Loading rules…</div>
      {:else if rulesError}
        <div class="error">{rulesError}</div>
      {:else if rules.length === 0}
        <div class="empty">No rules registered. Start the sidecar to register rules.</div>
      {:else}
        <div class="rules-list">
          {#each rules as rule}
            {@const ana = analytics[rule.rule_id] ?? {}}
            <div class="rule-card" class:disabled={!rule.enabled} class:shadow={rule.shadow_mode}>
              <div class="rule-row" on:click={() => expandRule(rule)}>
                <div class="rule-left">
                  <span class="rule-id">{rule.rule_id}</span>
                  <span class="sidecar-tag">{rule.sidecar_name} · {rule.sidecar_kind}</span>
                </div>
                <div class="rule-right">
                  {#if rule.shadow_mode}
                    <span class="pill pill-shadow">shadow</span>
                  {/if}
                  {#if ana.firing_count}
                    <span class="pill pill-count">{ana.firing_count} fires</span>
                  {/if}
                  {#if ana.last_fired_ns}
                    <span class="pill pill-ts">last {fmtTs(ana.last_fired_ns)}</span>
                  {/if}
                  <button class="toggle-btn" class:on={rule.enabled}
                    on:click|stopPropagation={() => toggleRule(rule)}>
                    {rule.enabled ? 'Enabled' : 'Disabled'}
                  </button>
                  <button class="shadow-btn" class:active={rule.shadow_mode}
                    on:click|stopPropagation={() => toggleShadow(rule)}
                    title="Toggle shadow mode (fires but doesn't alert)">
                    ◑ Shadow
                  </button>
                  <span class="chevron">{expandedRule === rule.rule_id ? '▲' : '▼'}</span>
                </div>
              </div>

              {#if expandedRule === rule.rule_id}
                <div class="rule-detail">
                  <div class="detail-section">
                    <label class="detail-label">Parameters (JSON)</label>
                    <textarea class="params-textarea"
                      bind:value={paramsEditing[rule.rule_id]}
                      rows="5"
                      spellcheck="false"
                    ></textarea>
                    <button class="btn-primary" on:click={() => saveParams(rule)}>Save Parameters</button>
                    <p class="hint">Changes apply on next override poll cycle (≤60s).</p>
                  </div>

                  {#if rule.shadow_mode}
                    <div class="detail-section">
                      <label class="detail-label">Shadow Firings
                        <button class="btn-ghost sm" on:click={async () => {
                          const sf = await api.rules.shadowFirings(rule.rule_id);
                          shadowFirings[rule.rule_id] = sf?.shadow_firings ?? [];
                        }}>↻</button>
                      </label>
                      {#if (shadowFirings[rule.rule_id] ?? []).length === 0}
                        <div class="empty">No shadow firings yet.</div>
                      {:else}
                        <table class="sf-table">
                          <thead><tr><th>Time</th><th>Device</th><th>Reason</th></tr></thead>
                          <tbody>
                            {#each (shadowFirings[rule.rule_id] ?? []).slice(-20).reverse() as sf}
                              <tr>
                                <td class="ts-cell">{fmtTs(sf.fired_at_ns)}</td>
                                <td class="dev-cell">{sf.device_address}</td>
                                <td class="reason-cell">{sf.reason}</td>
                              </tr>
                            {/each}
                          </tbody>
                        </table>
                      {/if}
                    </div>
                  {/if}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </section>

  <!-- ── Playbooks Tab ──────────────────────────────────────────────────── -->
  {:else if tab === 'playbooks'}
    <section class="tab-content">
      <div class="section-hdr">
        <span class="section-title">Playbooks <span class="badge">{playbooks.length}</span></span>
        <button class="btn-primary" on:click={() => showNewPlaybook = !showNewPlaybook}>+ New</button>
        <button class="btn-ghost" on:click={loadPlaybooks}>↻ Refresh</button>
      </div>

      {#if showNewPlaybook}
        <div class="new-form">
          <h3>New Playbook</h3>
          <label>Name<input class="form-input" bind:value={newPb.name} placeholder="e.g. BGP remediation"/></label>
          <label>Description<input class="form-input" bind:value={newPb.description} placeholder="Short description"/></label>
          <label>Steps JSON<textarea class="params-textarea" bind:value={newPb.steps_json} rows="5" spellcheck="false"></textarea></label>
          <div class="form-row">
            <button class="btn-primary" disabled={pbSaving} on:click={saveNewPlaybook}>{pbSaving ? 'Saving…' : 'Create'}</button>
            <button class="btn-ghost" on:click={() => showNewPlaybook = false}>Cancel</button>
          </div>
        </div>
      {/if}

      {#if playbooksLoading}
        <div class="loading">Loading playbooks…</div>
      {:else if playbooksError}
        <div class="error">{playbooksError}</div>
      {:else if playbooks.length === 0}
        <div class="empty">No playbooks yet.</div>
      {:else}
        <table class="data-table">
          <thead>
            <tr>
              <th>Name</th><th>Version</th><th>Enabled</th>
              <th>Executions</th><th>Last Run</th><th></th>
            </tr>
          </thead>
          <tbody>
            {#each playbooks as pb}
              {@const st = playbookStats[pb.id] ?? {}}
              <tr>
                <td>
                  <div class="pb-name">{pb.name}</div>
                  {#if pb.description}<div class="pb-desc">{pb.description}</div>{/if}
                </td>
                <td class="mono">v{pb.version}</td>
                <td><span class="pill" class:pill-on={pb.enabled} class:pill-off={!pb.enabled}>{pb.enabled ? 'on' : 'off'}</span></td>
                <td class="num-cell">{st.execution_count ?? pb.execution_count ?? 0}</td>
                <td class="ts-cell">{fmtTs(st.last_executed_at_ns ?? pb.last_executed_at_ns)}</td>
                <td>
                  <button class="btn-danger sm" on:click={() => deletePlaybook(pb)}>Delete</button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>

  <!-- ── Syslog Patterns Tab ────────────────────────────────────────────── -->
  {:else if tab === 'syslog'}
    <section class="tab-content">
      <div class="section-hdr">
        <span class="section-title">Custom Syslog Patterns <span class="badge">{syslogRules.length}</span></span>
        <button class="btn-primary" on:click={() => showNewSyslog = !showNewSyslog}>+ New</button>
        <button class="btn-ghost" on:click={loadSyslogRules}>↻ Refresh</button>
      </div>

      {#if showNewSyslog}
        <div class="new-form">
          <h3>New Syslog Pattern Rule</h3>
          <label>Rule ID <span class="req">*</span><input class="form-input" bind:value={newSyslog.rule_id} placeholder="e.g. syslog_ospf_drop"/></label>
          <label>Description<input class="form-input" bind:value={newSyslog.description} placeholder="What this rule detects"/></label>
          <label>Pattern (regex) <span class="req">*</span><input class="form-input" bind:value={newSyslog.pattern} placeholder="e.g. OSPF.*neighbor.*down"/></label>
          <label>Event Type<input class="form-input" bind:value={newSyslog.event_type} placeholder="syslog_match"/></label>
          <label>Severity
            <select class="form-select" bind:value={newSyslog.severity}>
              <option value="info">info</option>
              <option value="warn">warn</option>
              <option value="high">high</option>
              <option value="critical">critical</option>
            </select>
          </label>
          <label>Vendor (optional)<input class="form-input" bind:value={newSyslog.vendor} placeholder="cisco_iosxr"/></label>
          <label class="check-label">
            <input type="checkbox" bind:checked={newSyslog.shadow_mode}> Start in shadow mode
          </label>
          <div class="form-row">
            <button class="btn-primary" disabled={syslogSaving} on:click={saveNewSyslogRule}>{syslogSaving ? 'Saving…' : 'Create'}</button>
            <button class="btn-ghost" on:click={() => showNewSyslog = false}>Cancel</button>
          </div>
        </div>
      {/if}

      {#if syslogLoading}
        <div class="loading">Loading…</div>
      {:else if syslogRules.length === 0}
        <div class="empty">No custom syslog pattern rules. Click "+ New" to create one.</div>
      {:else}
        <table class="data-table">
          <thead>
            <tr><th>Rule ID</th><th>Pattern</th><th>Severity</th><th>Vendor</th><th>Shadow</th><th>Enabled</th></tr>
          </thead>
          <tbody>
            {#each syslogRules as sr}
              <tr>
                <td class="mono">{sr.rule_id}</td>
                <td class="mono pattern-cell">{sr.pattern}</td>
                <td><span class="sev-badge {severityClass(sr.severity)}">{sr.severity}</span></td>
                <td>{sr.vendor || '—'}</td>
                <td>{sr.shadow_mode ? '◑' : ''}</td>
                <td><span class="pill" class:pill-on={sr.enabled !== false} class:pill-off={sr.enabled === false}>{sr.enabled !== false ? 'on' : 'off'}</span></td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  {/if}
</div>

<style>
  .rules-page { display: flex; flex-direction: column; height: 100%; gap: 0; }
  .page-hdr { padding: 1.25rem 1.5rem 0; border-bottom: 1px solid var(--border, #e2e8f0); }
  .page-hdr h1 { margin: 0 0 0.75rem; font-size: 1.25rem; font-weight: 600; color: var(--text, #1a202c); }
  .tab-bar { display: flex; gap: 0; }
  .tab-btn { padding: 0.5rem 1.25rem; border: none; border-bottom: 3px solid transparent; background: none;
    cursor: pointer; font-size: 0.875rem; color: var(--text-muted, #718096); }
  .tab-btn.active { border-bottom-color: var(--accent, #4f46e5); color: var(--accent, #4f46e5); font-weight: 600; }
  .tab-content { flex: 1; overflow-y: auto; padding: 1.25rem 1.5rem; }
  .section-hdr { display: flex; align-items: center; gap: 0.75rem; margin-bottom: 1rem; }
  .section-title { font-size: 0.9375rem; font-weight: 600; }
  .badge { display: inline-block; background: var(--bg2, #edf2f7); color: var(--text-muted, #718096);
    border-radius: 10px; padding: 0 0.45rem; font-size: 0.75rem; margin-left: 0.25rem; }

  /* Rule cards */
  .rules-list { display: flex; flex-direction: column; gap: 0.5rem; }
  .rule-card { border: 1px solid var(--border, #e2e8f0); border-radius: 8px; background: var(--card, #fff);
    overflow: hidden; transition: opacity 0.15s; }
  .rule-card.disabled { opacity: 0.55; }
  .rule-card.shadow { border-left: 3px solid #d97706; }
  .rule-row { display: flex; align-items: center; justify-content: space-between;
    padding: 0.75rem 1rem; cursor: pointer; gap: 1rem; }
  .rule-row:hover { background: var(--bg2, #f7fafc); }
  .rule-left { display: flex; flex-direction: column; gap: 0.2rem; min-width: 0; }
  .rule-id { font-family: monospace; font-size: 0.875rem; font-weight: 600; color: var(--text, #1a202c); }
  .sidecar-tag { font-size: 0.75rem; color: var(--text-muted, #718096); }
  .rule-right { display: flex; align-items: center; gap: 0.5rem; flex-shrink: 0; flex-wrap: wrap; }
  .chevron { font-size: 0.7rem; color: var(--text-muted); }

  /* Pills */
  .pill { display: inline-block; border-radius: 12px; padding: 0.1rem 0.55rem; font-size: 0.72rem; font-weight: 500; }
  .pill-shadow { background: #fef3c7; color: #92400e; }
  .pill-count { background: #ede9fe; color: #5b21b6; }
  .pill-ts { background: var(--bg2, #edf2f7); color: var(--text-muted, #718096); }
  .pill-on { background: #dcfce7; color: #166534; }
  .pill-off { background: #fee2e2; color: #991b1b; }

  /* Buttons */
  .toggle-btn { padding: 0.25rem 0.75rem; border-radius: 6px; border: 1px solid #d1d5db;
    background: #f3f4f6; color: #374151; font-size: 0.8rem; cursor: pointer; }
  .toggle-btn.on { background: #dcfce7; border-color: #16a34a; color: #166534; }
  .shadow-btn { padding: 0.25rem 0.6rem; border-radius: 6px; border: 1px solid #d97706;
    background: none; color: #d97706; font-size: 0.8rem; cursor: pointer; }
  .shadow-btn.active { background: #fef3c7; }
  .btn-primary { padding: 0.35rem 1rem; border-radius: 6px; border: none;
    background: var(--accent, #4f46e5); color: #fff; font-size: 0.875rem; cursor: pointer; }
  .btn-primary:disabled { opacity: 0.6; cursor: default; }
  .btn-ghost { padding: 0.35rem 0.75rem; border-radius: 6px; border: 1px solid var(--border, #e2e8f0);
    background: none; color: var(--text-muted, #718096); font-size: 0.875rem; cursor: pointer; }
  .btn-ghost.sm { padding: 0.15rem 0.5rem; font-size: 0.75rem; }
  .btn-danger { padding: 0.25rem 0.6rem; border-radius: 6px; border: 1px solid #fca5a5;
    background: none; color: #dc2626; font-size: 0.8rem; cursor: pointer; }
  .btn-danger.sm { padding: 0.15rem 0.5rem; }

  /* Rule detail panel */
  .rule-detail { border-top: 1px solid var(--border, #e2e8f0); padding: 1rem; display: flex; flex-wrap: wrap; gap: 1.25rem; }
  .detail-section { flex: 1; min-width: 260px; display: flex; flex-direction: column; gap: 0.5rem; }
  .detail-label { font-size: 0.8125rem; font-weight: 600; color: var(--text-muted, #718096); }
  .params-textarea { font-family: monospace; font-size: 0.8125rem; border: 1px solid var(--border, #e2e8f0);
    border-radius: 6px; padding: 0.5rem; resize: vertical; width: 100%; box-sizing: border-box; }
  .hint { font-size: 0.75rem; color: var(--text-muted, #718096); margin: 0; }

  /* Shadow firings table */
  .sf-table { width: 100%; border-collapse: collapse; font-size: 0.8125rem; }
  .sf-table th { text-align: left; padding: 0.35rem 0.5rem; background: var(--bg2, #f7fafc);
    border-bottom: 1px solid var(--border, #e2e8f0); font-weight: 600; }
  .sf-table td { padding: 0.3rem 0.5rem; border-bottom: 1px solid var(--border, #e2e8f0); }

  /* Data table (playbooks/syslog) */
  .data-table { width: 100%; border-collapse: collapse; font-size: 0.875rem; }
  .data-table th { text-align: left; padding: 0.5rem 0.75rem; background: var(--bg2, #f7fafc);
    border-bottom: 1px solid var(--border, #e2e8f0); font-weight: 600; color: var(--text-muted, #718096); }
  .data-table td { padding: 0.5rem 0.75rem; border-bottom: 1px solid var(--border, #e2e8f0); vertical-align: middle; }
  .data-table tr:hover td { background: var(--bg2, #f7fafc); }
  .pb-name { font-weight: 600; }
  .pb-desc { font-size: 0.75rem; color: var(--text-muted, #718096); }
  .mono { font-family: monospace; }
  .pattern-cell { max-width: 280px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ts-cell { font-size: 0.8rem; color: var(--text-muted, #718096); white-space: nowrap; }
  .num-cell { text-align: right; font-variant-numeric: tabular-nums; }

  /* Severity badges */
  .sev-badge { display: inline-block; border-radius: 4px; padding: 0.1rem 0.45rem; font-size: 0.72rem; font-weight: 600; }
  .sev-crit { background: #fee2e2; color: #991b1b; }
  .sev-high { background: #ffedd5; color: #9a3412; }
  .sev-warn { background: #fef9c3; color: #854d0e; }
  .sev-info { background: #e0f2fe; color: #0c4a6e; }

  /* New form */
  .new-form { background: var(--bg2, #f7fafc); border: 1px solid var(--border, #e2e8f0);
    border-radius: 8px; padding: 1.25rem; margin-bottom: 1.25rem;
    display: flex; flex-direction: column; gap: 0.75rem; max-width: 640px; }
  .new-form h3 { margin: 0; font-size: 1rem; }
  .new-form label { display: flex; flex-direction: column; gap: 0.25rem; font-size: 0.875rem; font-weight: 500; }
  .form-input { border: 1px solid var(--border, #e2e8f0); border-radius: 6px; padding: 0.4rem 0.75rem;
    font-size: 0.875rem; background: #fff; }
  .form-select { border: 1px solid var(--border, #e2e8f0); border-radius: 6px; padding: 0.4rem 0.75rem;
    font-size: 0.875rem; background: #fff; }
  .check-label { flex-direction: row !important; align-items: center; gap: 0.5rem; font-weight: normal; }
  .form-row { display: flex; gap: 0.75rem; }
  .req { color: #dc2626; }

  .loading { color: var(--text-muted, #718096); padding: 2rem; text-align: center; }
  .error { color: #dc2626; padding: 1rem; background: #fee2e2; border-radius: 6px; }
  .empty { color: var(--text-muted, #718096); padding: 2rem; text-align: center; font-style: italic; }
</style>
