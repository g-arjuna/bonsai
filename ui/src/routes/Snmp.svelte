<script>
  import { onMount } from 'svelte';

  let tab = $state('receiver');
  let loading = $state(true);
  let error = $state('');
  let saveMsg = $state('');
  let saving = $state(false);

  // Receiver config
  let streaming = $state(null);

  // Shun rules (OID patterns also live here)
  let shunRules = $state([]);
  let shunLoading = $state(false);
  let shunError = $state('');

  // Live receiver status
  let receiverStatus = $state(null);

  // New shun rule form
  let newShun = $state({ scope_type: 'global', scope_value: '', match_type: 'fact_type', match_value: '', action: 'drop', rate_limit_per_min: 60 });

  onMount(async () => {
    await Promise.all([loadStreaming(), loadReceiverStatus()]);
    loading = false;
  });

  async function loadStreaming() {
    try {
      const r = await fetch('/api/settings/streaming');
      if (!r.ok) throw new Error(await r.text());
      streaming = await r.json();
    } catch (e) { error = e.message; }
  }

  async function loadReceiverStatus() {
    try {
      const r = await fetch('/api/receivers/status');
      if (r.ok) receiverStatus = await r.json();
    } catch {}
  }

  async function loadShunRules() {
    shunLoading = true;
    try {
      const r = await fetch('/api/shun/rules');
      if (!r.ok) throw new Error(await r.text());
      const d = await r.json();
      shunRules = d.rules ?? [];
    } catch (e) { shunError = e.message; }
    finally { shunLoading = false; }
  }

  $effect(() => {
    if (tab === 'shun') loadShunRules();
  });

  async function saveStreaming() {
    saving = true; saveMsg = '';
    try {
      const r = await fetch('/api/settings/streaming', {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(streaming),
      });
      const res = await r.json();
      if (!r.ok) throw new Error(res.error || 'Save failed');
      saveMsg = res.note || 'Saved';
    } catch (e) { saveMsg = e.message; }
    finally { saving = false; }
  }

  async function createShunRule() {
    try {
      const r = await fetch('/api/shun/rules', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(newShun),
      });
      if (!r.ok) throw new Error(await r.text());
      newShun = { scope_type: 'global', scope_value: '', match_type: 'fact_type', match_value: '', action: 'drop', rate_limit_per_min: 60 };
      await loadShunRules();
    } catch (e) { shunError = e.message; }
  }

  async function deleteShunRule(id) {
    try {
      await fetch(`/api/shun/rules/${encodeURIComponent(id)}`, { method: 'DELETE' });
      await loadShunRules();
    } catch {}
  }

  function snmpEnabled(s) {
    return s?.snmp?.enabled ?? false;
  }

  function snmpBind(s) {
    return s?.snmp?.bind_addr ?? '0.0.0.0:162';
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Signal Receivers</p>
      <h2>SNMP &amp; OID Management</h2>
    </div>
    {#if receiverStatus}
      {@const snmpSt = receiverStatus.receivers?.find(r => r.name === 'snmp')}
      <span class="status-badge" class:green={snmpSt?.running} class:red={!snmpSt?.running}>
        SNMP {snmpSt?.running ? 'running' : 'stopped'}
      </span>
    {/if}
  </div>

  <div class="tab-bar">
    {#each ['receiver', 'shun', 'oids'] as t}
      <button class="tab-btn" class:active={tab === t} onclick={() => (tab = t)}>
        {t === 'receiver' ? 'Receiver Config' : t === 'shun' ? 'Shun Rules' : 'OID Patterns'}
      </button>
    {/each}
  </div>

  {#if loading}
    <p class="muted">Loading…</p>
  {:else if error}
    <p class="error-msg">{error}</p>
  {:else if tab === 'receiver'}
    <!-- ── Receiver Config ── -->
    {#if streaming}
      <div class="form-section">
        <h3>SNMP Trap Receiver</h3>
        <label class="field-row">
          <span>Enabled</span>
          <input type="checkbox" bind:checked={streaming.snmp.enabled} />
        </label>
        <label class="field-row">
          <span>Bind address</span>
          <input class="mono-input" bind:value={streaming.snmp.bind_addr} placeholder="0.0.0.0:162" />
        </label>
        <label class="field-row">
          <span>Community allowlist (comma-separated, empty = accept all)</span>
          <input class="mono-input" bind:value={streaming.snmp.community_allowlist_csv} placeholder="public,monitoring" />
        </label>
        <label class="field-row">
          <span>Dedup window (ms)</span>
          <input type="number" bind:value={streaming.snmp.dedup_window_ms} min="0" max="60000" />
        </label>
      </div>

      <div class="form-section">
        <h3>SNMPv2c</h3>
        <label class="field-row">
          <span>Enabled</span>
          <input type="checkbox" bind:checked={streaming.snmp.v2c_enabled} />
        </label>
      </div>

      <div class="form-section">
        <h3>SNMPv3 USM Users</h3>
        <p class="muted small">Keys are stored in vault — never in plaintext config. Configure via <code>BONSAI_VAULT_PASSPHRASE</code>.</p>
        {#if (streaming.snmp.v3_users ?? []).length === 0}
          <p class="muted small">No v3 users configured.</p>
        {:else}
          <table class="data-table">
            <thead><tr><th>Security Name</th><th>Auth Proto</th><th>Priv Proto</th></tr></thead>
            <tbody>
              {#each streaming.snmp.v3_users as u}
                <tr>
                  <td><code>{u.security_name}</code></td>
                  <td>{u.auth_protocol ?? '—'}</td>
                  <td>{u.priv_protocol ?? 'none'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
        <p class="muted small">Add v3 users via <code>bonsai.toml</code> <code>[[signals.snmp.v3_users]]</code> section.</p>
      </div>

      <div class="action-row">
        <button class="primary" onclick={saveStreaming} disabled={saving}>
          {saving ? 'Saving…' : 'Save Changes'}
        </button>
        {#if saveMsg}<span class="save-msg">{saveMsg}</span>{/if}
      </div>
    {/if}

  {:else if tab === 'shun'}
    <!-- ── Shun Rules ── -->
    <div class="form-section">
      <h3>Add Shun Rule</h3>
      <div class="shun-form">
        <label>
          Scope
          <select bind:value={newShun.scope_type}>
            <option value="global">Global</option>
            <option value="device">Device</option>
            <option value="site">Site</option>
          </select>
        </label>
        {#if newShun.scope_type !== 'global'}
          <label>
            Scope Value
            <input bind:value={newShun.scope_value} placeholder="device address / site name" />
          </label>
        {/if}
        <label>
          Match Type
          <select bind:value={newShun.match_type}>
            <option value="fact_type">Fact Type</option>
            <option value="substring">Substring</option>
            <option value="regex">Regex</option>
          </select>
        </label>
        <label>
          Match Value
          <input class="mono-input" bind:value={newShun.match_value} placeholder="e.g. bgp_neighbor_down or LICC:" />
        </label>
        <label>
          Action
          <select bind:value={newShun.action}>
            <option value="drop">Drop</option>
            <option value="rate_limit">Rate Limit</option>
          </select>
        </label>
        {#if newShun.action === 'rate_limit'}
          <label>
            Rate limit (per min)
            <input type="number" bind:value={newShun.rate_limit_per_min} min="1" max="10000" />
          </label>
        {/if}
        <button class="primary" onclick={createShunRule}>Add Rule</button>
      </div>
      {#if shunError}<p class="error-msg">{shunError}</p>{/if}
    </div>

    {#if shunLoading}
      <p class="muted">Loading rules…</p>
    {:else if shunRules.length === 0}
      <p class="muted">No shun rules configured.</p>
    {:else}
      <table class="data-table">
        <thead><tr><th>Scope</th><th>Match</th><th>Action</th><th>Created</th><th></th></tr></thead>
        <tbody>
          {#each shunRules as rule}
            <tr>
              <td>{rule.scope_type}{rule.scope_value ? ': ' + rule.scope_value : ''}</td>
              <td><code>{rule.match_type}:{rule.match_value}</code></td>
              <td><span class="action-badge action-{rule.action}">{rule.action}</span></td>
              <td class="muted small">{rule.created_by ?? '—'}</td>
              <td><button class="ghost-sm danger" onclick={() => deleteShunRule(rule.id)}>Delete</button></td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

  {:else if tab === 'oids'}
    <!-- ── OID Patterns ── -->
    <div class="form-section">
      <h3>OID Pattern Library</h3>
      <p class="muted small">
        OID patterns are loaded from <code>config/snmp_oid_patterns/</code> YAML files and stored in the ConfigItem DB.
        Upload a vendor MIB below to auto-generate patterns.
      </p>
      <div class="oid-info">
        <div class="oid-info-row">
          <span class="oid-vendor">Nokia TIMETRA-BGP-MIB</span>
          <span class="oid-detail">BGP peer address via OID index suffix (last 4 octets → IPv4)</span>
          <span class="oid-status bundled">bundled</span>
        </div>
        <div class="oid-info-row">
          <span class="oid-vendor">Cisco CISCO-BGP4-MIB</span>
          <span class="oid-detail">BGP neighbor state traps</span>
          <span class="oid-status bundled">bundled</span>
        </div>
        <div class="oid-info-row">
          <span class="oid-vendor">RFC 2863 IF-MIB</span>
          <span class="oid-detail">Interface linkDown / linkUp</span>
          <span class="oid-status bundled">bundled</span>
        </div>
      </div>
      <p class="muted small" style="margin-top:12px">
        Custom MIB upload (requires <code>pysmi</code> on the server): coming in D4-1 T4 MIB compile pipeline.
      </p>
    </div>
  {/if}
</div>

<style>
  .tab-bar { display: flex; gap: 4px; border-bottom: 1px solid var(--border-subtle); margin-bottom: 20px; }
  .tab-btn {
    padding: 7px 14px; background: none; border: none; border-bottom: 2px solid transparent;
    color: var(--text-secondary); cursor: pointer; font-size: 13px; font-family: inherit;
    transition: color 0.15s, border-color 0.15s;
  }
  .tab-btn:hover { color: var(--text-primary); }
  .tab-btn.active { color: var(--accent-primary, #58a6ff); border-bottom-color: var(--accent-primary, #58a6ff); }

  .status-badge { font-size: 11px; font-weight: 700; padding: 3px 10px; border-radius: 12px; align-self: center; }
  .status-badge.green { background: rgba(34,197,94,0.12); color: #22c55e; border: 1px solid rgba(34,197,94,0.3); }
  .status-badge.red { background: rgba(239,68,68,0.12); color: #ef4444; border: 1px solid rgba(239,68,68,0.3); }

  .form-section { background: var(--bg-surface); border: 1px solid var(--border-subtle); border-radius: 6px; padding: 16px 20px; margin-bottom: 16px; }
  .form-section h3 { margin: 0 0 12px; font-size: 13px; font-weight: 600; }

  .field-row { display: flex; align-items: center; gap: 16px; padding: 5px 0; font-size: 13px; color: var(--text-secondary); }
  .field-row span { width: 260px; flex-shrink: 0; }
  .mono-input { font-family: var(--font-mono); font-size: 12px; }

  .shun-form { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 10px; align-items: end; }
  .shun-form label { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--text-secondary); }
  .shun-form input, .shun-form select { padding: 5px 8px; background: var(--bg-elevated); border: 1px solid var(--border-subtle); border-radius: 4px; color: var(--text-primary); font-size: 12px; font-family: inherit; }

  .action-row { display: flex; align-items: center; gap: 12px; margin-top: 8px; }
  .save-msg { font-size: 12px; color: var(--text-secondary); }

  .action-badge { font-size: 10px; font-weight: 600; text-transform: uppercase; padding: 1px 6px; border-radius: 3px; }
  .action-drop { background: rgba(239,68,68,0.12); color: #fca5a5; }
  .action-rate_limit { background: rgba(251,191,36,0.12); color: #fde68a; }

  .data-table { width: 100%; border-collapse: collapse; font-size: 12px; }
  .data-table th { text-align: left; padding: 6px 10px; border-bottom: 1px solid var(--border-subtle); font-size: 11px; text-transform: uppercase; color: var(--text-tertiary); font-weight: 600; }
  .data-table td { padding: 5px 10px; border-bottom: 1px solid var(--border-subtle); color: var(--text-secondary); }

  .ghost-sm { background: none; border: 1px solid var(--border-subtle); border-radius: 4px; padding: 2px 8px; font-size: 11px; cursor: pointer; color: var(--text-tertiary); }
  .ghost-sm.danger:hover { color: #fca5a5; border-color: rgba(239,68,68,0.4); }

  .oid-info { display: flex; flex-direction: column; gap: 6px; }
  .oid-info-row { display: flex; align-items: center; gap: 12px; font-size: 12px; padding: 4px 0; border-bottom: 1px solid var(--border-subtle); }
  .oid-vendor { font-family: var(--font-mono); font-size: 11px; color: var(--text-primary); width: 200px; flex-shrink: 0; }
  .oid-detail { flex: 1; color: var(--text-secondary); }
  .oid-status { font-size: 10px; font-weight: 600; text-transform: uppercase; padding: 1px 6px; border-radius: 3px; }
  .oid-status.bundled { background: rgba(88,166,255,0.1); color: #58a6ff; border: 1px solid rgba(88,166,255,0.25); }

  .muted { color: var(--text-tertiary); }
  .small { font-size: 11px; }
  .error-msg { color: #fca5a5; font-size: 12px; }
</style>
