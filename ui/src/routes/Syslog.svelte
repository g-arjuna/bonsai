<script>
  import { onMount } from 'svelte';
  import { relativeTime, absoluteTime, shortTime } from '$lib/timeutil.js';

  let tab = $state('shun');

  // ── Shun Rules (D4-2 T4) ──────────────────────────────────────────────────
  let rules = $state([]);
  let stats = $state({});
  let rulesLoading = $state(true);
  let rulesError = $state(null);
  let saving = $state(false);
  let saveError = $state(null);

  let form = $state({
    scope_type: 'global',
    scope_value: '',
    match_type: 'substring',
    match_value: '',
    action: 'drop',
    rate_limit_per_min: 60,
    expires_h: 0,
  });

  async function loadRules() {
    rulesLoading = true;
    try {
      const [rr, sr] = await Promise.all([
        fetch('/api/shun/rules').then(r => r.ok ? r.json() : Promise.reject(r.statusText)),
        fetch('/api/shun/stats').then(r => r.ok ? r.json() : { stats: {} }),
      ]);
      rules = rr.rules ?? [];
      stats = sr.stats ?? {};
      rulesError = null;
    } catch (e) {
      rulesError = String(e);
    } finally {
      rulesLoading = false;
    }
  }

  async function createRule() {
    if (!form.match_value.trim()) return;
    saving = true;
    saveError = null;
    try {
      const body = {
        scope_type: form.scope_type,
        scope_value: form.scope_value.trim(),
        match_type: form.match_type,
        match_value: form.match_value.trim(),
        action: form.action,
        rate_limit_per_min: form.action === 'rate_limit' ? Number(form.rate_limit_per_min) : 0,
        expires_at_ns: form.expires_h > 0
          ? (Date.now() + form.expires_h * 3_600_000) * 1_000_000
          : 0,
        created_by: 'ui',
      };
      const r = await fetch('/api/shun/rules', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!r.ok) throw new Error(await r.text());
      form.match_value = '';
      form.scope_value = '';
      await loadRules();
    } catch (e) {
      saveError = String(e);
    } finally {
      saving = false;
    }
  }

  async function disableRule(id) {
    await fetch(`/api/shun/rules/${encodeURIComponent(id)}/disable`, { method: 'POST' });
    await loadRules();
  }

  async function deleteRule(id) {
    await fetch(`/api/shun/rules/${encodeURIComponent(id)}/delete`, { method: 'POST' });
    await loadRules();
  }

  function silenceDevice(address) {
    form.scope_type = 'device';
    form.scope_value = address;
    form.match_type = 'substring';
    form.match_value = '';
    form.action = 'drop';
    form.expires_h = 1;
    tab = 'shun';
  }

  onMount(loadRules);
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Signal processing</p>
      <h2>Syslog &amp; Shun</h2>
    </div>
  </div>

  <!-- Tab bar -->
  <div class="tab-bar" role="tablist">
    <button
      role="tab"
      class:active={tab === 'shun'}
      onclick={() => tab = 'shun'}
    >Shun Rules</button>
    <button
      role="tab"
      class:active={tab === 'patterns'}
      onclick={() => tab = 'patterns'}
    >Patterns</button>
  </div>

  <!-- ── Shun Rules tab ──────────────────────────────────────────────────── -->
  {#if tab === 'shun'}
    <div class="shun-panel">

      <!-- Create form -->
      <section class="card form-card">
        <div class="card-header">New Shun Rule</div>
        <div class="form-grid">
          <label class="field">
            <span class="field-label">Scope</span>
            <select bind:value={form.scope_type}>
              <option value="global">Global</option>
              <option value="device">Device</option>
              <option value="site">Site</option>
              <option value="group">Group</option>
            </select>
          </label>
          {#if form.scope_type !== 'global'}
            <label class="field">
              <span class="field-label">{form.scope_type === 'device' ? 'Device address' : form.scope_type === 'site' ? 'Site name' : 'Group name'}</span>
              <input type="text" bind:value={form.scope_value} placeholder="e.g. 10.0.0.1" />
            </label>
          {/if}
          <label class="field">
            <span class="field-label">Match type</span>
            <select bind:value={form.match_type}>
              <option value="substring">Substring</option>
              <option value="regex">Regex</option>
              <option value="fact_type">Fact type</option>
            </select>
          </label>
          <label class="field field-wide">
            <span class="field-label">Match value</span>
            <input
              type="text"
              bind:value={form.match_value}
              placeholder="e.g. LICC: License warning"
            />
          </label>
          <label class="field">
            <span class="field-label">Action</span>
            <select bind:value={form.action}>
              <option value="drop">Drop</option>
              <option value="rate_limit">Rate limit</option>
            </select>
          </label>
          {#if form.action === 'rate_limit'}
            <label class="field">
              <span class="field-label">Limit (per min)</span>
              <input type="number" min="1" bind:value={form.rate_limit_per_min} />
            </label>
          {/if}
          <label class="field">
            <span class="field-label">Expires in</span>
            <select bind:value={form.expires_h}>
              <option value={0}>Never</option>
              <option value={1}>1 hour</option>
              <option value={24}>24 hours</option>
              <option value={168}>7 days</option>
            </select>
          </label>
        </div>
        {#if saveError}
          <div class="notice error">{saveError}</div>
        {/if}
        <div class="form-actions">
          <button
            class="btn-primary"
            onclick={createRule}
            disabled={saving || !form.match_value.trim()}
          >
            {saving ? 'Saving…' : 'Create Rule'}
          </button>
        </div>
      </section>

      <!-- Rules table -->
      <section class="card">
        <div class="card-header">
          Active Rules
          <button class="refresh-btn" onclick={loadRules} title="Refresh">↺</button>
        </div>
        {#if rulesLoading}
          <div class="empty-msg">Loading…</div>
        {:else if rulesError}
          <div class="notice error">{rulesError}</div>
        {:else if rules.length === 0}
          <div class="empty-msg">No shun rules configured.</div>
        {:else}
          <div class="rules-table">
            <div class="rules-header">
              <span>Scope</span>
              <span>Match</span>
              <span>Action</span>
              <span>Suppressed</span>
              <span>Status</span>
              <span></span>
            </div>
            {#each rules as rule}
              {@const suppressed = stats[rule.id] ?? 0}
              <div class="rule-row" class:disabled={!rule.enabled}>
                <div class="rule-scope">
                  <span class="scope-badge scope-{rule.scope_type}">{rule.scope_type}</span>
                  {#if rule.scope_value}
                    <code class="scope-val">{rule.scope_value}</code>
                  {/if}
                </div>
                <div class="rule-match">
                  <span class="match-type">{rule.match_type}</span>
                  <code class="match-val">{rule.match_value}</code>
                </div>
                <div class="rule-action">
                  <span class="action-badge action-{rule.action}">{rule.action}</span>
                  {#if rule.action === 'rate_limit' && rule.rate_limit_per_min > 0}
                    <span class="rate-val">{rule.rate_limit_per_min}/min</span>
                  {/if}
                </div>
                <div class="rule-count">
                  {#if suppressed > 0}
                    <span class="suppressed-count">{suppressed.toLocaleString()}</span>
                  {:else}
                    <span class="suppressed-zero">—</span>
                  {/if}
                </div>
                <div class="rule-status">
                  {#if rule.enabled}
                    <span class="status-active">active</span>
                  {:else}
                    <span class="status-disabled">disabled</span>
                  {/if}
                  {#if rule.expires_at_ns > 0}
                    <span class="expiry" title={absoluteTime(rule.expires_at_ns)}>
                      exp {relativeTime(rule.expires_at_ns)}
                    </span>
                  {/if}
                </div>
                <div class="rule-actions">
                  {#if rule.enabled}
                    <button class="row-btn" onclick={() => disableRule(rule.id)}>Disable</button>
                  {/if}
                  <button class="row-btn danger" onclick={() => deleteRule(rule.id)}>Delete</button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <!-- Seed patterns hint -->
      <section class="card seeds-card">
        <div class="card-header">Common Noise Patterns</div>
        <p class="seeds-hint">Click to pre-fill the form above.</p>
        <div class="seeds-list">
          {#each SEED_PATTERNS as seed}
            <button
              class="seed-btn"
              onclick={() => {
                form.scope_type = seed.scope_type;
                form.scope_value = '';
                form.match_type = seed.match_type;
                form.match_value = seed.match_value;
                form.action = seed.action;
                form.rate_limit_per_min = seed.rate_limit_per_min ?? 60;
                form.expires_h = 0;
              }}
            >
              <span class="seed-vendor">{seed.vendor}</span>
              <code class="seed-pattern">{seed.match_value}</code>
            </button>
          {/each}
        </div>
      </section>

    </div>

  <!-- ── Patterns tab (placeholder for D4-1 T7) ─────────────────────────── -->
  {:else if tab === 'patterns'}
    <div class="patterns-placeholder">
      <p>Syslog pattern management (D4-1 T7) — coming in a future batch.</p>
      <p class="hint">Pattern files are currently loaded from <code>config/syslog_patterns/*.yaml</code>.</p>
    </div>
  {/if}
</div>

<script context="module">
  const SEED_PATTERNS = [
    { vendor: 'Nokia SRL', match_type: 'substring', match_value: 'LICC: License warning', scope_type: 'global', action: 'drop' },
    { vendor: 'Nokia SRL', match_type: 'substring', match_value: 'LOGGER: Timed out waiting for sync', scope_type: 'global', action: 'drop' },
    { vendor: 'Cisco IOS', match_type: 'substring', match_value: '%SYS-5-CONFIG_I', scope_type: 'global', action: 'drop' },
    { vendor: 'FRR', match_type: 'substring', match_value: 'bgpd: %BGP-3-MAXPFXEXCEED', scope_type: 'global', action: 'rate_limit', rate_limit_per_min: 5 },
    { vendor: 'Nokia SRL', match_type: 'substring', match_value: 'CPU threshold exceeded', scope_type: 'global', action: 'rate_limit', rate_limit_per_min: 10 },
  ];
</script>

<style>
  /* ── View root ────────────────────────────────────────────────────────── */
  .view { min-width: 0; overflow-x: hidden; }

  /* ── Tab bar ──────────────────────────────────────────────────────────── */
  .tab-bar {
    display: flex;
    gap: 2px;
    border-bottom: 1px solid var(--border-subtle);
    margin-bottom: 14px;
  }
  .tab-bar button {
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    padding: 6px 14px;
    font-size: var(--text-small);
    color: var(--text-secondary);
    cursor: pointer;
    transition: color var(--duration-instant), border-color var(--duration-instant);
    margin-bottom: -1px;
  }
  .tab-bar button:hover { color: var(--text-primary); }
  .tab-bar button.active {
    color: var(--accent-primary);
    border-bottom-color: var(--accent-primary);
    font-weight: 600;
  }

  /* ── Cards ────────────────────────────────────────────────────────────── */
  .shun-panel { display: flex; flex-direction: column; gap: 12px; }
  .card {
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    overflow: hidden;
  }
  .card-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-tertiary);
    border-bottom: 1px solid var(--border-subtle);
  }
  .refresh-btn {
    background: none;
    border: none;
    color: var(--text-tertiary);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    margin-left: auto;
    padding: 0 2px;
  }
  .refresh-btn:hover { color: var(--text-primary); }

  /* ── Create form ──────────────────────────────────────────────────────── */
  .form-grid {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    padding: 12px 14px 8px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 140px;
  }
  .field-wide { flex: 2; min-width: 220px; }
  .field-label {
    font-size: 11px;
    color: var(--text-tertiary);
    letter-spacing: 0.03em;
  }
  .field input,
  .field select {
    font-size: var(--text-small);
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    color: var(--text-primary);
    padding: 4px 8px;
  }
  .form-actions {
    padding: 0 14px 12px;
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .btn-primary {
    background: var(--accent-primary);
    color: #000;
    border: none;
    border-radius: 4px;
    padding: 5px 14px;
    font-size: var(--text-small);
    font-weight: 600;
    cursor: pointer;
  }
  .btn-primary:disabled { opacity: 0.5; cursor: default; }

  /* ── Rules table ──────────────────────────────────────────────────────── */
  .rules-table { font-size: var(--text-small); }
  .rules-header {
    display: grid;
    grid-template-columns: 160px 1fr 120px 80px 110px 120px;
    gap: 8px;
    padding: 6px 14px;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-tertiary);
    border-bottom: 1px solid var(--border-subtle);
  }
  .rule-row {
    display: grid;
    grid-template-columns: 160px 1fr 120px 80px 110px 120px;
    gap: 8px;
    padding: 7px 14px;
    border-bottom: 1px solid var(--border-subtle);
    align-items: center;
    transition: background var(--duration-instant);
  }
  .rule-row:last-child { border-bottom: none; }
  .rule-row:hover { background: var(--bg-elevated); }
  .rule-row.disabled { opacity: 0.45; }

  .rule-scope { display: flex; align-items: center; gap: 6px; min-width: 0; }
  .scope-badge {
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    padding: 1px 5px;
    border-radius: 3px;
    letter-spacing: 0.03em;
    white-space: nowrap;
  }
  .scope-global { background: rgba(99,102,241,0.12); color: #a5b4fc; }
  .scope-device { background: rgba(77,208,200,0.10); color: #4dd0c8; }
  .scope-site   { background: rgba(251,146,60,0.12); color: #fdba74; }
  .scope-group  { background: rgba(167,139,250,0.12); color: #a78bfa; }
  .scope-val {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-secondary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rule-match { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .match-type {
    font-size: 10px;
    color: var(--text-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .match-val {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--text-primary);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rule-action { display: flex; align-items: center; gap: 6px; }
  .action-badge {
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    padding: 1px 5px;
    border-radius: 3px;
    letter-spacing: 0.03em;
    white-space: nowrap;
  }
  .action-drop       { background: rgba(239,68,68,0.12); color: #fca5a5; }
  .action-rate_limit { background: rgba(251,191,36,0.12); color: #fde68a; }
  .rate-val { font-size: 11px; color: var(--text-tertiary); }

  .suppressed-count { font-weight: 600; color: var(--text-primary); }
  .suppressed-zero  { color: var(--text-tertiary); }

  .rule-status { display: flex; flex-direction: column; gap: 2px; }
  .status-active   { font-size: 10px; color: #4ade80; font-weight: 600; text-transform: uppercase; letter-spacing: 0.03em; }
  .status-disabled { font-size: 10px; color: var(--text-tertiary); font-weight: 600; text-transform: uppercase; letter-spacing: 0.03em; }
  .expiry { font-size: 10px; color: var(--text-tertiary); }

  .rule-actions { display: flex; gap: 6px; }
  .row-btn {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 3px;
    border: 1px solid var(--border-subtle);
    background: var(--bg-elevated);
    color: var(--text-secondary);
    cursor: pointer;
    white-space: nowrap;
  }
  .row-btn:hover { color: var(--text-primary); border-color: var(--border-default); }
  .row-btn.danger { color: #fca5a5; }
  .row-btn.danger:hover { border-color: rgba(239,68,68,0.4); background: rgba(239,68,68,0.08); }

  /* ── Seed patterns ────────────────────────────────────────────────────── */
  .seeds-card .seeds-hint {
    padding: 6px 14px 0;
    font-size: var(--text-small);
    color: var(--text-tertiary);
  }
  .seeds-list {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 8px 14px 12px;
  }
  .seed-btn {
    display: flex;
    align-items: center;
    gap: 6px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: 4px;
    padding: 4px 10px;
    cursor: pointer;
    font-size: 11px;
    color: var(--text-secondary);
    transition: border-color var(--duration-instant), color var(--duration-instant);
  }
  .seed-btn:hover { border-color: var(--accent-primary); color: var(--text-primary); }
  .seed-vendor {
    font-size: 10px;
    font-weight: 600;
    color: var(--text-tertiary);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .seed-pattern { font-family: var(--font-mono); font-size: 11px; }

  /* ── Patterns placeholder ─────────────────────────────────────────────── */
  .patterns-placeholder {
    padding: 24px;
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    color: var(--text-secondary);
    font-size: var(--text-small);
    line-height: 1.6;
  }
  .patterns-placeholder .hint { margin-top: 8px; color: var(--text-tertiary); }

  /* ── Misc ─────────────────────────────────────────────────────────────── */
  .empty-msg { padding: 16px 14px; color: var(--text-secondary); font-size: var(--text-small); }
  .notice.error {
    margin: 8px 14px;
    padding: 6px 10px;
    background: rgba(239,68,68,0.08);
    border: 1px solid rgba(239,68,68,0.25);
    border-radius: 4px;
    color: #fca5a5;
    font-size: var(--text-small);
  }
</style>
