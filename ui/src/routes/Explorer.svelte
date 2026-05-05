<script>
  import { onMount } from 'svelte';

  // ── state ──────────────────────────────────────────────────────────────────

  let cypher = $state('MATCH (d:Device) RETURN d.address, d.hostname, d.vendor ORDER BY d.hostname LIMIT 25');
  let running = $state(false);
  let error = $state(null);
  let result = $state(null);  // ExplorerResult | null

  let savedQueries = $state([]);
  let saveModalOpen = $state(false);
  let saveName = $state('');
  let saveDescription = $state('');
  let saving = $state(false);

  let insights = $state(null);
  let insightsLoading = $state(false);
  let activeTab = $state('explorer'); // 'explorer' | 'insights'

  // Curated query library shown in the sidebar
  const QUERY_LIBRARY = [
    {
      label: 'All devices',
      cypher: 'MATCH (d:Device) RETURN d.address, d.hostname, d.vendor ORDER BY d.hostname',
    },
    {
      label: 'Devices in DC environment',
      cypher: "MATCH (env:Environment {archetype: 'data_center'})<-[:BELONGS_TO_ENVIRONMENT]-(s:Site)<-[:LOCATED_AT]-(d:Device) RETURN d.address, d.hostname, d.vendor, s.name ORDER BY d.hostname",
    },
    {
      label: 'Active detections (last 24 h)',
      cypher: "MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) RETURN d.address, de.rule_id, de.severity, de.fired_at ORDER BY de.fired_at DESC LIMIT 50",
    },
    {
      label: 'Devices missing enrichment',
      cypher: 'MATCH (d:Device) OPTIONAL MATCH (d)-[:HAS_ENRICHMENT_PROPERTY]->(ep:EnrichmentProperty) WITH d, count(ep) AS ep_count WHERE ep_count = 0 RETURN d.address, d.hostname ORDER BY d.address',
    },
    {
      label: 'Unresolved detections',
      cypher: 'MATCH (de:DetectionEvent) OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(de) WITH de, r WHERE r IS NULL RETURN de.id, de.device_address, de.rule_id, de.severity ORDER BY de.fired_at DESC LIMIT 50',
    },
    {
      label: 'Applications per site',
      cypher: 'MATCH (s:Site)<-[:LOCATED_AT]-(d:Device)-[:RUNS_SERVICE|CARRIES_APPLICATION]->(a:Application) RETURN DISTINCT a.name, d.hostname, s.name ORDER BY s.name, a.name',
    },
    {
      label: 'Topology neighbors of device',
      cypher: "MATCH (d:Device {address: '10.0.0.1'})-[:HAS_INTERFACE]->(si:Interface)-[:CONNECTED_TO]-(di:Interface)<-[:HAS_INTERFACE]-(nb:Device) RETURN DISTINCT nb.address, nb.hostname, si.name AS local_if, di.name AS remote_if",
    },
    {
      label: 'Subscription health per device',
      cypher: "MATCH (d:Device)-[:HAS_SUBSCRIPTION_STATUS]->(ss:SubscriptionStatus) RETURN d.hostname, ss.path, ss.status ORDER BY d.hostname, ss.path",
    },
    {
      label: 'Co-firing detections (all time)',
      cypher: "MATCH (d:Device)-[:TRIGGERED]->(e1:DetectionEvent) MATCH (d)-[:TRIGGERED]->(e2:DetectionEvent) WHERE e1.rule_id < e2.rule_id RETURN e1.rule_id, e2.rule_id, count(DISTINCT d.address) AS co_count ORDER BY co_count DESC LIMIT 20",
    },
    {
      label: 'Enrichment properties (NetBox)',
      cypher: "MATCH (d:Device)-[:HAS_ENRICHMENT_PROPERTY]->(ep:EnrichmentProperty) WHERE ep.source_name = 'netbox' RETURN d.hostname, ep.key, ep.value ORDER BY d.hostname",
    },
    {
      label: 'Sites per environment',
      cypher: "MATCH (e:Environment)<-[:BELONGS_TO_ENVIRONMENT]-(s:Site) RETURN e.name, e.archetype, s.name ORDER BY e.name, s.name",
    },
    {
      label: 'Remediation history',
      cypher: 'MATCH (r:Remediation)-[:RESOLVES]->(de:DetectionEvent) RETURN r.id, de.rule_id, r.action, r.status, r.attempted_at ORDER BY r.attempted_at DESC LIMIT 30',
    },
  ];

  onMount(() => {
    loadSavedQueries();
  });

  // ── query execution ────────────────────────────────────────────────────────

  async function runQuery(queryText, savedQueryId) {
    running = true;
    error = null;
    result = null;
    if (queryText !== undefined) cypher = queryText;
    try {
      const body = { cypher };
      if (savedQueryId) body.saved_query_id = savedQueryId;
      const r = await fetch('/api/explorer/query', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!r.ok) {
        const msg = await r.text();
        throw new Error(msg);
      }
      result = await r.json();
      if (savedQueryId) loadSavedQueries(); // refresh last_run_at
    } catch (e) {
      error = e.message;
    } finally {
      running = false;
    }
  }

  function pickLibraryQuery(q) {
    cypher = q.cypher;
    result = null;
    error = null;
  }

  // ── saved queries ──────────────────────────────────────────────────────────

  async function loadSavedQueries() {
    try {
      const r = await fetch('/api/explorer/saved-queries');
      if (r.ok) savedQueries = await r.json();
    } catch {}
  }

  function openSaveModal() {
    saveName = '';
    saveDescription = '';
    saveModalOpen = true;
  }

  async function confirmSave() {
    if (!saveName.trim()) return;
    saving = true;
    try {
      const r = await fetch('/api/explorer/saved-queries', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: saveName, description: saveDescription, cypher }),
      });
      if (!r.ok) throw new Error(await r.text());
      saveModalOpen = false;
      loadSavedQueries();
    } catch (e) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  async function deleteSavedQuery(id) {
    await fetch(`/api/explorer/saved-queries/${id}/delete`, { method: 'POST' });
    loadSavedQueries();
  }

  // ── graph insights ─────────────────────────────────────────────────────────

  async function loadInsights() {
    if (insights) return; // cached for the session
    insightsLoading = true;
    try {
      const r = await fetch('/api/graph/insights');
      if (r.ok) insights = await r.json();
    } catch {}
    insightsLoading = false;
  }

  function switchTab(tab) {
    activeTab = tab;
    if (tab === 'insights' && !insights) loadInsights();
  }

  // ── formatting helpers ─────────────────────────────────────────────────────

  function cellStr(v) {
    if (v === null || v === undefined) return 'null';
    if (typeof v === 'object') return JSON.stringify(v);
    return String(v);
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Graph</p>
      <h1>Explorer</h1>
    </div>
    <div class="tab-bar">
      <button class="tab-btn" class:active={activeTab === 'explorer'} onclick={() => switchTab('explorer')}>Query</button>
      <button class="tab-btn" class:active={activeTab === 'insights'} onclick={() => switchTab('insights')}>Insights</button>
    </div>
  </div>

  {#if activeTab === 'explorer'}
    <!-- ── explorer pane ─────────────────────────────────────────────────── -->
    <div class="explorer-layout">
      <!-- sidebar -->
      <aside class="query-sidebar">
        <div class="sidebar-section">
          <div class="sidebar-section-title">Query library</div>
          {#each QUERY_LIBRARY as q}
            <button class="lib-btn" onclick={() => pickLibraryQuery(q)}>{q.label}</button>
          {/each}
        </div>

        {#if savedQueries.length > 0}
          <div class="sidebar-section">
            <div class="sidebar-section-title">Saved queries</div>
            {#each savedQueries as sq}
              <div class="saved-row">
                <button class="lib-btn saved-run-btn" onclick={() => runQuery(sq.cypher, sq.id)}>
                  {sq.name}
                </button>
                <button class="icon-btn delete-btn" onclick={() => deleteSavedQuery(sq.id)} title="Delete">×</button>
              </div>
              {#if sq.description}
                <div class="saved-desc">{sq.description}</div>
              {/if}
              {#if sq.last_result_count > 0}
                <div class="saved-meta">{sq.last_result_count} rows last run</div>
              {/if}
            {/each}
          </div>
        {/if}
      </aside>

      <!-- main editor + results -->
      <div class="editor-area">
        <div class="editor-toolbar">
          <span class="editor-label">Cypher (read-only — mutations are rejected)</span>
          <div class="editor-actions">
            {#if result}
              <button class="btn-secondary" onclick={openSaveModal}>Save query</button>
            {/if}
            <button class="btn-primary" onclick={() => runQuery()} disabled={running}>
              {running ? 'Running…' : 'Run'}
            </button>
          </div>
        </div>

        <textarea
          class="cypher-input"
          bind:value={cypher}
          rows="6"
          spellcheck="false"
          onkeydown={(e) => { if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') { e.preventDefault(); runQuery(); } }}
          placeholder="MATCH (d:Device) RETURN d.address, d.hostname LIMIT 10"
        ></textarea>

        {#if error}
          <div class="error-banner">{error}</div>
        {/if}

        {#if result}
          <div class="result-meta">
            {result.row_count} row{result.row_count !== 1 ? 's' : ''}
            {#if result.truncated} <span class="truncated-badge">(truncated at 500)</span>{/if}
          </div>

          {#if result.rows.length > 0}
            <div class="result-table-wrap">
              <table class="result-table">
                <thead>
                  <tr>
                    {#each result.columns as col}
                      <th>{col}</th>
                    {/each}
                  </tr>
                </thead>
                <tbody>
                  {#each result.rows as row}
                    <tr>
                      {#each row as cell}
                        <td>{cellStr(cell)}</td>
                      {/each}
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {:else}
            <div class="empty-result">Query returned 0 rows.</div>
          {/if}
        {/if}
      </div>
    </div>

    <!-- ── save modal ────────────────────────────────────────────────────── -->
    {#if saveModalOpen}
      <div class="modal-backdrop" onclick={() => { saveModalOpen = false; }}>
        <div class="modal" onclick={(e) => e.stopPropagation()}>
          <h2 class="modal-title">Save query</h2>
          <label class="field-label">Name
            <input class="field-input" bind:value={saveName} placeholder="My query name" />
          </label>
          <label class="field-label">Description (optional)
            <input class="field-input" bind:value={saveDescription} placeholder="What does this query show?" />
          </label>
          <div class="modal-actions">
            <button class="btn-secondary" onclick={() => { saveModalOpen = false; }}>Cancel</button>
            <button class="btn-primary" onclick={confirmSave} disabled={saving || !saveName.trim()}>
              {saving ? 'Saving…' : 'Save'}
            </button>
          </div>
        </div>
      </div>
    {/if}

  {:else}
    <!-- ── insights pane ─────────────────────────────────────────────────── -->
    {#if insightsLoading}
      <div class="loading">Computing graph insights…</div>
    {:else if !insights}
      <div class="empty">Could not load insights.</div>
    {:else}
      <div class="insights-grid">

        <!-- device centrality -->
        <div class="insight-card wide">
          <div class="card-title">Device centrality <span class="card-subtitle">— degree (physical links)</span></div>
          <table class="result-table">
            <thead><tr><th>Device</th><th>Hostname</th><th>Degree</th></tr></thead>
            <tbody>
              {#each insights.device_centrality as row}
                <tr>
                  <td class="mono">{row.address}</td>
                  <td>{row.hostname}</td>
                  <td>
                    <div class="degree-bar-wrap">
                      <div class="degree-bar" style="width: {Math.min(row.degree * 12, 120)}px"></div>
                      <span>{row.degree}</span>
                    </div>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>

        <!-- site dependency -->
        <div class="insight-card">
          <div class="card-title">Site dependencies</div>
          <table class="result-table">
            <thead><tr><th>Site</th><th>Local devices</th><th>Cross-site reach</th></tr></thead>
            <tbody>
              {#each insights.site_dependencies as row}
                <tr>
                  <td>{row.site_name}</td>
                  <td>{row.local_device_count}</td>
                  <td>{row.reachable_cross_site}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>

        <!-- detection correlation -->
        <div class="insight-card">
          <div class="card-title">Detection co-firing pairs</div>
          {#if insights.detection_correlations.length === 0}
            <div class="empty-result">No co-firing pairs detected.</div>
          {:else}
            <table class="result-table">
              <thead><tr><th>Rule A</th><th>Rule B</th><th>Co-fires</th></tr></thead>
              <tbody>
                {#each insights.detection_correlations as row}
                  <tr>
                    <td class="mono">{row.rule_a}</td>
                    <td class="mono">{row.rule_b}</td>
                    <td>{row.co_fire_count}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </div>

        <!-- subscription health by tier -->
        <div class="insight-card">
          <div class="card-title">Subscription health by tier</div>
          <table class="result-table">
            <thead><tr><th>Tier</th><th>Devices</th><th>Active subs</th><th>Unmonitored</th></tr></thead>
            <tbody>
              {#each insights.tier_health as row}
                <tr class:warn-row={row.unmonitored_devices > 0 && row.device_count > 0}>
                  <td>{row.tier}</td>
                  <td>{row.device_count}</td>
                  <td>{row.active_subscriptions}</td>
                  <td class:critical-cell={row.unmonitored_devices > 0}>{row.unmonitored_devices}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>

        <!-- orphan count -->
        <div class="insight-card narrow">
          <div class="card-title">Orphan devices</div>
          <div class="stat-big {insights.orphan_count > 0 ? 'critical' : 'healthy'}">{insights.orphan_count}</div>
          <div class="stat-label">devices with no topology neighbours</div>
        </div>

      </div>
    {/if}
  {/if}
</div>

<style>
  /* ── layout ──────────────────────────────────────────────────────────────── */
  .workspace-header {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    margin-bottom: 1.25rem;
    gap: 1rem;
  }
  .eyebrow { font-size: 0.7rem; text-transform: uppercase; letter-spacing: 0.08em; color: var(--text-muted, #888); margin: 0 0 0.15rem; }
  h1 { margin: 0; font-size: 1.4rem; }

  /* ── tabs ────────────────────────────────────────────────────────────────── */
  .tab-bar { display: flex; gap: 0.25rem; }
  .tab-btn {
    padding: 0.35rem 0.9rem;
    border: 1px solid var(--border, #333);
    border-radius: 4px;
    background: transparent;
    color: var(--text-muted, #888);
    cursor: pointer;
    font-size: 0.85rem;
  }
  .tab-btn.active {
    background: var(--accent, #3b82f6);
    color: #fff;
    border-color: var(--accent, #3b82f6);
  }

  /* ── explorer layout ─────────────────────────────────────────────────────── */
  .explorer-layout { display: flex; gap: 1rem; align-items: flex-start; }

  .query-sidebar {
    width: 220px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }

  .sidebar-section { display: flex; flex-direction: column; gap: 0.2rem; }
  .sidebar-section-title {
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--text-muted, #888);
    padding: 0.3rem 0 0.15rem;
  }

  .lib-btn {
    text-align: left;
    background: transparent;
    border: none;
    color: var(--text, #ccc);
    padding: 0.3rem 0.4rem;
    border-radius: 3px;
    cursor: pointer;
    font-size: 0.8rem;
    line-height: 1.3;
    width: 100%;
  }
  .lib-btn:hover { background: var(--surface-hover, #2a2a2a); }

  .saved-row { display: flex; gap: 0.25rem; align-items: center; }
  .saved-run-btn { flex: 1; }
  .icon-btn {
    background: transparent;
    border: none;
    cursor: pointer;
    color: var(--text-muted, #888);
    font-size: 1rem;
    padding: 0 0.25rem;
    line-height: 1;
  }
  .icon-btn:hover { color: var(--critical, #ef4444); }
  .saved-desc { font-size: 0.72rem; color: var(--text-muted, #888); padding: 0 0.4rem; }
  .saved-meta { font-size: 0.7rem; color: var(--text-muted, #888); padding: 0 0.4rem 0.3rem; }

  /* ── editor ──────────────────────────────────────────────────────────────── */
  .editor-area { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 0.75rem; }

  .editor-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .editor-label { font-size: 0.75rem; color: var(--text-muted, #888); }
  .editor-actions { display: flex; gap: 0.5rem; }

  .cypher-input {
    width: 100%;
    box-sizing: border-box;
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 4px;
    color: var(--text, #e5e5e5);
    font-family: 'JetBrains Mono', 'Fira Code', monospace;
    font-size: 0.82rem;
    line-height: 1.5;
    padding: 0.6rem 0.75rem;
    resize: vertical;
  }
  .cypher-input:focus { outline: 1px solid var(--accent, #3b82f6); border-color: var(--accent, #3b82f6); }

  .btn-primary {
    padding: 0.35rem 0.85rem;
    border: none;
    border-radius: 4px;
    background: var(--accent, #3b82f6);
    color: #fff;
    cursor: pointer;
    font-size: 0.83rem;
  }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-secondary {
    padding: 0.35rem 0.85rem;
    border: 1px solid var(--border, #333);
    border-radius: 4px;
    background: transparent;
    color: var(--text, #ccc);
    cursor: pointer;
    font-size: 0.83rem;
  }

  .error-banner {
    background: color-mix(in srgb, var(--critical, #ef4444) 15%, transparent);
    border: 1px solid var(--critical, #ef4444);
    border-radius: 4px;
    padding: 0.5rem 0.75rem;
    font-size: 0.82rem;
    color: var(--critical, #ef4444);
  }

  .result-meta { font-size: 0.75rem; color: var(--text-muted, #888); }
  .truncated-badge { color: var(--warn, #f59e0b); }

  /* ── results table ───────────────────────────────────────────────────────── */
  .result-table-wrap { overflow-x: auto; }
  .result-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.8rem;
  }
  .result-table th {
    text-align: left;
    padding: 0.35rem 0.6rem;
    background: var(--surface, #1a1a1a);
    border-bottom: 1px solid var(--border, #333);
    color: var(--text-muted, #888);
    font-weight: 500;
    white-space: nowrap;
  }
  .result-table td {
    padding: 0.3rem 0.6rem;
    border-bottom: 1px solid color-mix(in srgb, var(--border, #333) 50%, transparent);
    color: var(--text, #e5e5e5);
    max-width: 320px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .result-table tr:hover td { background: var(--surface-hover, #2a2a2a); }
  .mono { font-family: 'JetBrains Mono', monospace; font-size: 0.75rem; }

  .empty-result { padding: 1rem 0; color: var(--text-muted, #888); font-size: 0.85rem; }

  /* ── save modal ──────────────────────────────────────────────────────────── */
  .modal-backdrop {
    position: fixed; inset: 0;
    background: rgba(0,0,0,0.55);
    z-index: 100;
    display: flex; align-items: center; justify-content: center;
  }
  .modal {
    background: var(--surface, #1e1e1e);
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    padding: 1.5rem;
    width: 420px;
    max-width: 95vw;
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
  }
  .modal-title { margin: 0; font-size: 1rem; }
  .field-label { display: flex; flex-direction: column; gap: 0.25rem; font-size: 0.8rem; color: var(--text-muted, #888); }
  .field-input {
    padding: 0.4rem 0.6rem;
    background: var(--surface, #111);
    border: 1px solid var(--border, #333);
    border-radius: 4px;
    color: var(--text, #e5e5e5);
    font-size: 0.85rem;
  }
  .modal-actions { display: flex; justify-content: flex-end; gap: 0.5rem; }

  /* ── insights ────────────────────────────────────────────────────────────── */
  .insights-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1rem;
    align-items: start;
  }
  .insight-card {
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    padding: 1rem;
  }
  .insight-card.wide { grid-column: 1 / -1; }
  .insight-card.narrow { grid-column: span 1; }
  .card-title { font-size: 0.85rem; font-weight: 600; margin-bottom: 0.75rem; }
  .card-subtitle { font-weight: 400; color: var(--text-muted, #888); }

  .degree-bar-wrap { display: flex; align-items: center; gap: 0.5rem; }
  .degree-bar { height: 6px; border-radius: 3px; background: var(--accent, #3b82f6); min-width: 4px; }

  .warn-row td { background: color-mix(in srgb, var(--warn, #f59e0b) 6%, transparent); }
  .critical-cell { color: var(--critical, #ef4444); font-weight: 600; }

  .stat-big { font-size: 2.5rem; font-weight: 700; line-height: 1; margin: 0.5rem 0 0.25rem; }
  .stat-big.critical { color: var(--critical, #ef4444); }
  .stat-big.healthy { color: var(--healthy, #22c55e); }
  .stat-label { font-size: 0.78rem; color: var(--text-muted, #888); }

  .loading { padding: 2rem; text-align: center; color: var(--text-muted, #888); }
  .empty { padding: 2rem; text-align: center; color: var(--text-muted, #888); }
</style>
