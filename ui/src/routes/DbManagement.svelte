<script>
  import { onMount } from 'svelte';

  let tab = $state('stats');
  let stats = $state(null);
  let schema = $state(null);
  let loadingStats = $state(true);
  let loadingSchema = $state(false);
  let statsError = $state('');
  let schemaError = $state('');

  // Manage tab state
  let purgeNodeType = $state('DetectionEvent');
  let purgeOlderDays = $state(90);
  let purgeResult = $state(null);
  let purgeLoading = $state(false);
  let checkpointResult = $state(null);
  let checkpointLoading = $state(false);
  let exportNodeType = $state('Device');
  let exportLimit = $state(10000);

  // Backups tab state
  let backups = $state([]);
  let backupsLoading = $state(false);
  let backupCreating = $state(false);
  let backupResult = $state(null);

  const purgeable = ['DetectionEvent', 'AppFlow', 'AgentToolCall', 'InvestigationFeedback', 'GnnScore', 'StateChangeEvent'];
  const exportable = ['Device', 'Interface', 'BgpSession', 'IsIsAdj', 'BfdSession', 'DetectionEvent', 'Incident', 'AppFlow', 'Application', 'Investigation', 'AgentToolCall', 'ConfigChange', 'ChangeRequest', 'Location', 'Prefix', 'HostEndpoint', 'ShunRule'];

  onMount(loadStats);

  async function loadStats() {
    loadingStats = true;
    statsError = '';
    try {
      const r = await fetch('/api/db/stats');
      if (!r.ok) throw new Error(await r.text());
      stats = await r.json();
    } catch (e) {
      statsError = e.message;
    } finally {
      loadingStats = false;
    }
  }

  async function loadSchema() {
    if (schema) return;
    loadingSchema = true;
    schemaError = '';
    try {
      const r = await fetch('/api/db/schema');
      if (!r.ok) throw new Error(await r.text());
      schema = await r.json();
    } catch (e) {
      schemaError = e.message;
    } finally {
      loadingSchema = false;
    }
  }

  async function doPurge() {
    if (!confirm(`Delete ${purgeNodeType} nodes older than ${purgeOlderDays} days? This cannot be undone.`)) return;
    purgeLoading = true;
    purgeResult = null;
    try {
      const r = await fetch(`/api/db/purge?node_type=${purgeNodeType}&older_than_days=${purgeOlderDays}`, { method: 'DELETE' });
      if (!r.ok) throw new Error(await r.text());
      purgeResult = await r.json();
      loadStats();
    } catch (e) {
      purgeResult = { error: e.message };
    } finally {
      purgeLoading = false;
    }
  }

  async function doCheckpoint() {
    checkpointLoading = true;
    checkpointResult = null;
    try {
      const r = await fetch('/api/db/checkpoint', { method: 'POST' });
      if (!r.ok) throw new Error(await r.text());
      checkpointResult = await r.json();
    } catch (e) {
      checkpointResult = { error: e.message };
    } finally {
      checkpointLoading = false;
    }
  }

  function doExport() {
    window.open(`/api/db/export?node_type=${exportNodeType}&limit=${exportLimit}`, '_blank');
  }

  async function loadBackups() {
    backupsLoading = true;
    try {
      const r = await fetch('/api/db/backups');
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      backups = data.backups || [];
    } catch (e) {
      backups = [];
    } finally {
      backupsLoading = false;
    }
  }

  async function createBackup() {
    backupCreating = true;
    backupResult = null;
    try {
      const r = await fetch('/api/db/backup', { method: 'POST' });
      if (!r.ok) throw new Error(await r.text());
      backupResult = await r.json();
      loadBackups();
    } catch (e) {
      backupResult = { error: e.message };
    } finally {
      backupCreating = false;
    }
  }

  function switchTab(t) {
    tab = t;
    if (t === 'schema') loadSchema();
    if (t === 'backups') loadBackups();
  }

  function fmtBytes(b) {
    if (!b) return '0 B';
    if (b < 1024) return b + ' B';
    if (b < 1024 * 1024) return (b / 1024).toFixed(1) + ' KB';
    if (b < 1024 * 1024 * 1024) return (b / (1024 * 1024)).toFixed(1) + ' MB';
    return (b / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
  }

  function sortedEntries(obj) {
    return Object.entries(obj || {}).sort((a, b) => b[1] - a[1]);
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Administration</p>
      <h2>Database</h2>
    </div>
    <button class="primary" onclick={loadStats}>Refresh</button>
  </div>

  <div class="tab-bar">
    <button class:active={tab === 'stats'} onclick={() => switchTab('stats')}>Stats</button>
    <button class:active={tab === 'schema'} onclick={() => switchTab('schema')}>Schema</button>
    <button class:active={tab === 'manage'} onclick={() => switchTab('manage')}>Manage</button>
    <button class:active={tab === 'backups'} onclick={() => switchTab('backups')}>Backups</button>
  </div>

  {#if tab === 'stats'}
    {#if loadingStats}
      <p class="muted">Loading stats...</p>
    {:else if statsError}
      <p class="error">{statsError}</p>
    {:else if stats}
      <div class="stats-grid">
        <div class="stat-card">
          <span class="stat-value">{fmtBytes(stats.db_size_bytes)}</span>
          <span class="stat-label">DB Size</span>
        </div>
        <div class="stat-card">
          <span class="stat-value">{stats.total_node_tables}</span>
          <span class="stat-label">Node Tables</span>
        </div>
        <div class="stat-card">
          <span class="stat-value">{stats.total_rel_tables}</span>
          <span class="stat-label">Rel Tables</span>
        </div>
      </div>

      <div class="two-col">
        <div>
          <h3>Node Counts</h3>
          <table class="data-table">
            <thead><tr><th>Table</th><th>Count</th></tr></thead>
            <tbody>
              {#each sortedEntries(stats.node_counts) as [name, count]}
                <tr><td class="mono">{name}</td><td class="num">{count.toLocaleString()}</td></tr>
              {/each}
              {#if Object.keys(stats.node_counts).length === 0}
                <tr><td colspan="2" class="muted">No data</td></tr>
              {/if}
            </tbody>
          </table>
        </div>
        <div>
          <h3>Relationship Counts</h3>
          <table class="data-table">
            <thead><tr><th>Table</th><th>Count</th></tr></thead>
            <tbody>
              {#each sortedEntries(stats.rel_counts) as [name, count]}
                <tr><td class="mono">{name}</td><td class="num">{count.toLocaleString()}</td></tr>
              {/each}
              {#if Object.keys(stats.rel_counts).length === 0}
                <tr><td colspan="2" class="muted">No data</td></tr>
              {/if}
            </tbody>
          </table>
        </div>
      </div>
    {/if}

  {:else if tab === 'schema'}
    {#if loadingSchema}
      <p class="muted">Loading schema...</p>
    {:else if schemaError}
      <p class="error">{schemaError}</p>
    {:else if schema}
      <h3>Node Tables ({schema.node_tables.length})</h3>
      {#each schema.node_tables as table}
        <details class="schema-table">
          <summary><strong>{table.name}</strong> <span class="muted">({table.columns.length} columns)</span></summary>
          <table class="data-table">
            <thead><tr><th>Column</th><th>Type</th></tr></thead>
            <tbody>
              {#each table.columns as col}
                <tr><td class="mono">{col.name}</td><td class="mono type">{col.type}</td></tr>
              {/each}
            </tbody>
          </table>
        </details>
      {/each}

      <h3 style="margin-top: 24px">Relationship Tables ({schema.rel_tables.length})</h3>
      {#each schema.rel_tables as table}
        <details class="schema-table">
          <summary><strong>{table.name}</strong> <span class="muted">({table.columns.length} columns)</span></summary>
          <table class="data-table">
            <thead><tr><th>Column</th><th>Type</th></tr></thead>
            <tbody>
              {#each table.columns as col}
                <tr><td class="mono">{col.name}</td><td class="mono type">{col.type}</td></tr>
              {/each}
            </tbody>
          </table>
        </details>
      {/each}
    {/if}

  {:else if tab === 'manage'}
    <div class="manage-section">
      <h3>Purge Old Data</h3>
      <p class="muted" style="margin-bottom:12px">Remove stale nodes older than a specified number of days. Connected edges are also removed.</p>
      <div class="form-row">
        <label>
          Node type
          <select bind:value={purgeNodeType}>
            {#each purgeable as t}<option value={t}>{t}</option>{/each}
          </select>
        </label>
        <label>
          Older than (days)
          <input type="number" bind:value={purgeOlderDays} min="1" max="3650" style="width:80px" />
        </label>
        <button class="danger" onclick={doPurge} disabled={purgeLoading}>
          {purgeLoading ? 'Purging...' : 'Purge'}
        </button>
      </div>
      {#if purgeResult}
        {#if purgeResult.error}
          <p class="error">{purgeResult.error}</p>
        {:else}
          <p class="success">Deleted {purgeResult.deleted_count} {purgeResult.node_type} nodes older than {purgeResult.older_than_days} days.</p>
        {/if}
      {/if}
    </div>

    <div class="manage-section">
      <h3>WAL Checkpoint</h3>
      <p class="muted" style="margin-bottom:12px">Force a KuzuDB WAL flush and compaction.</p>
      <button class="primary" onclick={doCheckpoint} disabled={checkpointLoading}>
        {checkpointLoading ? 'Running...' : 'Run Checkpoint'}
      </button>
      {#if checkpointResult}
        {#if checkpointResult.error}
          <p class="error">{checkpointResult.error}</p>
        {:else}
          <p class="success">Checkpoint complete.</p>
        {/if}
      {/if}
    </div>

    <div class="manage-section">
      <h3>Export Nodes</h3>
      <p class="muted" style="margin-bottom:12px">Download node data as JSONL file.</p>
      <div class="form-row">
        <label>
          Node type
          <select bind:value={exportNodeType}>
            {#each exportable as t}<option value={t}>{t}</option>{/each}
          </select>
        </label>
        <label>
          Limit
          <input type="number" bind:value={exportLimit} min="1" max="100000" style="width:100px" />
        </label>
        <button class="primary" onclick={doExport}>Download JSONL</button>
      </div>
    </div>

  {:else if tab === 'backups'}
    <div class="manage-section">
      <h3>Create Backup</h3>
      <p class="muted" style="margin-bottom:12px">Create a tar.gz snapshot of the runtime/ directory.</p>
      <button class="primary" onclick={createBackup} disabled={backupCreating}>
        {backupCreating ? 'Creating...' : 'Create Backup'}
      </button>
      {#if backupResult}
        {#if backupResult.error}
          <p class="error">{backupResult.error}</p>
        {:else}
          <p class="success">Backup created: {backupResult.filename}</p>
        {/if}
      {/if}
    </div>

    <div class="manage-section">
      <h3>Existing Backups</h3>
      {#if backupsLoading}
        <p class="muted">Loading...</p>
      {:else if backups.length === 0}
        <p class="muted">No backups found.</p>
      {:else}
        <table class="data-table">
          <thead><tr><th>Filename</th><th>Size</th></tr></thead>
          <tbody>
            {#each backups as b}
              <tr><td class="mono">{b.filename}</td><td class="num">{fmtBytes(b.size_bytes)}</td></tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  {/if}
</div>

<style>
  .tab-bar { display: flex; gap: 4px; margin-bottom: 16px; border-bottom: 1px solid var(--border); padding-bottom: 8px; }
  .tab-bar button { background: none; border: none; color: var(--text-muted); cursor: pointer; padding: 6px 14px; border-radius: 4px 4px 0 0; font-size: 13px; }
  .tab-bar button.active { color: var(--text); background: var(--surface-hover); font-weight: 600; }

  .stats-grid { display: flex; gap: 16px; margin-bottom: 24px; flex-wrap: wrap; }
  .stat-card { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 16px 24px; min-width: 140px; text-align: center; }
  .stat-value { display: block; font-size: 22px; font-weight: 700; color: var(--accent, #60a5fa); }
  .stat-label { display: block; font-size: 11px; color: var(--text-muted); margin-top: 4px; text-transform: uppercase; letter-spacing: 0.5px; }

  .two-col { display: grid; grid-template-columns: 1fr 1fr; gap: 24px; }
  @media (max-width: 800px) { .two-col { grid-template-columns: 1fr; } }

  .data-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .data-table th { text-align: left; border-bottom: 1px solid var(--border); padding: 6px 10px; font-weight: 600; color: var(--text-muted); font-size: 11px; text-transform: uppercase; }
  .data-table td { padding: 5px 10px; border-bottom: 1px solid var(--border-light, rgba(255,255,255,0.05)); }
  .mono { font-family: 'SF Mono', monospace; font-size: 12px; }
  .num { text-align: right; font-variant-numeric: tabular-nums; }
  .type { color: var(--text-muted); }

  .schema-table { margin-bottom: 8px; }
  .schema-table summary { cursor: pointer; padding: 6px 8px; border-radius: 4px; }
  .schema-table summary:hover { background: var(--surface-hover); }
  .schema-table[open] summary { margin-bottom: 6px; }

  .error { color: #ef4444; }
  .success { color: #22c55e; margin-top: 8px; }
  h3 { margin: 0 0 12px; font-size: 14px; }

  .manage-section { background: var(--surface); border: 1px solid var(--border); border-radius: 8px; padding: 20px; margin-bottom: 16px; }
  .form-row { display: flex; gap: 12px; align-items: flex-end; flex-wrap: wrap; }
  .form-row label { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--text-muted); }
  .form-row select, .form-row input { padding: 6px 10px; border-radius: 4px; border: 1px solid var(--border); background: var(--surface-hover); color: var(--text); font-size: 13px; }
  .danger { background: #dc2626; color: #fff; border: none; padding: 6px 16px; border-radius: 4px; cursor: pointer; font-size: 13px; }
  .danger:hover { background: #b91c1c; }
  .danger:disabled { opacity: 0.5; cursor: not-allowed; }
</style>
