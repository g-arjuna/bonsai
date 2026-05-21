<script>
  import { onMount } from 'svelte';

  let tab = $state('stats');
  let stats = $state(null);
  let schema = $state(null);
  let loadingStats = $state(true);
  let loadingSchema = $state(false);
  let statsError = $state('');
  let schemaError = $state('');

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

  function switchTab(t) {
    tab = t;
    if (t === 'schema') loadSchema();
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
  h3 { margin: 0 0 12px; font-size: 14px; }
</style>
