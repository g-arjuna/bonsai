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
  let insightsError = $state(null);
  let quality = $state(null);
  let qualityLoading = $state(false);
  let qualityError = $state(null);
  let activeTab = $state('ask'); // 'ask' | 'explorer' | 'insights' | 'health'

  // ── NL query state ──────────────────────────────────────────────────────────
  let nlQuestion = $state('');
  let nlRunning = $state(false);
  let nlResult = $state(null);  // AskResponse | null
  let nlError = $state(null);
  let nlBudget = $state(null);
  let nlHistory = $state([]);   // past Q&A pairs

  // Curated query library shown in the sidebar
  const QUERY_LIBRARY = [
    { section: 'Core' },
    {
      label: 'All devices',
      cypher: 'MATCH (d:Device) RETURN d.address, d.hostname, d.vendor, d.role, d.site ORDER BY d.hostname',
    },
    {
      label: 'Devices by vendor',
      cypher: 'MATCH (d:Device) RETURN d.vendor, count(d) AS device_count ORDER BY device_count DESC',
    },
    {
      label: 'Interfaces with errors',
      cypher: 'MATCH (d:Device)-[:HAS_INTERFACE]->(i:Interface) WHERE i.in_errors > 0 OR i.out_errors > 0 RETURN d.hostname, i.name, i.in_errors, i.out_errors, i.oper_status ORDER BY (i.in_errors + i.out_errors) DESC LIMIT 25',
    },
    {
      label: 'Topology links (LLDP)',
      cypher: 'MATCH (d:Device)-[:HAS_INTERFACE]->(si:Interface)-[:CONNECTED_TO]-(ri:Interface)<-[:HAS_INTERFACE]-(nb:Device) RETURN DISTINCT d.hostname, si.name AS local_if, ri.name AS remote_if, nb.hostname AS neighbor ORDER BY d.hostname',
    },
    { section: 'Routing & Sessions' },
    {
      label: 'BGP sessions',
      cypher: 'MATCH (d:Device)-[:PEERS_WITH]->(n:BgpNeighbor) RETURN d.hostname, n.peer_address, n.session_state, n.peer_as ORDER BY d.hostname',
    },
    {
      label: 'BGP sessions down',
      cypher: "MATCH (d:Device)-[:PEERS_WITH]->(n:BgpNeighbor) WHERE n.session_state <> 'established' RETURN d.hostname, n.peer_address, n.session_state, n.peer_as ORDER BY d.hostname",
    },
    {
      label: 'BMP sessions',
      cypher: 'MATCH (d:Device)-[:HAS_BMP_SESSION]->(b:BmpSession) RETURN d.hostname, b.peer_address, b.session_state, b.adj_rib_in_routes, b.loc_rib_routes ORDER BY d.hostname',
    },
    {
      label: 'ISIS adjacencies',
      cypher: 'MATCH (d:Device)-[:HAS_ISIS_ADJACENCY]->(ia:IsisAdjacency) RETURN d.hostname, ia.system_id, ia.adjacency_state ORDER BY d.hostname',
    },
    {
      label: 'OSPF neighbors',
      cypher: 'MATCH (d:Device)-[:HAS_OSPF_NEIGHBOR]->(on:OspfNeighbor) RETURN d.hostname, on.neighbor_id, on.area, on.state ORDER BY d.hostname',
    },
    { section: 'Detections & Events' },
    {
      label: 'Recent detections',
      cypher: 'MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) RETURN d.hostname, de.rule_id, de.severity, de.fired_at ORDER BY de.fired_at DESC LIMIT 50',
    },
    {
      label: 'Unresolved detections',
      cypher: 'MATCH (d:Device)-[:TRIGGERED]->(de:DetectionEvent) OPTIONAL MATCH (r:Remediation)-[:RESOLVES]->(de) WITH d, de, r WHERE r IS NULL RETURN d.hostname, de.rule_id, de.severity, de.fired_at ORDER BY de.fired_at DESC LIMIT 50',
    },
    {
      label: 'Recent syslog events',
      cypher: "MATCH (d:Device)-[:REPORTED_BY]->(e:StateChangeEvent) WHERE e.source_type = 'syslog' RETURN d.hostname, e.event_type, e.detail, e.occurred_at ORDER BY e.occurred_at DESC LIMIT 30",
    },
    {
      label: 'Remediation history',
      cypher: 'MATCH (r:Remediation)-[:RESOLVES]->(de:DetectionEvent) RETURN r.id, de.rule_id, r.action, r.status, r.attempted_at ORDER BY r.attempted_at DESC LIMIT 30',
    },
    { section: 'Monitoring' },
    {
      label: 'Subscription health',
      cypher: 'MATCH (d:Device)-[:HAS_SUBSCRIPTION_STATUS]->(ss:SubscriptionStatus) RETURN d.hostname, ss.path, ss.status ORDER BY d.hostname, ss.path',
    },
    {
      label: 'Sites & locations',
      cypher: 'MATCH (s:Site) OPTIONAL MATCH (d:Device)-[:LOCATED_AT]->(s) RETURN s.name, s.location, count(d) AS device_count ORDER BY s.name',
    },
    {
      label: 'Redundancy groups',
      cypher: 'MATCH (d:Device)-[m:MEMBER_OF]->(rg:RedundancyGroup) RETURN rg.name, rg.group_type, rg.state, d.hostname, m.role ORDER BY rg.group_type, rg.name',
    },
    { section: 'Enrichment' },
    {
      label: 'Enrichment properties',
      cypher: 'MATCH (d:Device)-[:HAS_ENRICHMENT_PROPERTY]->(ep:EnrichmentProperty) RETURN d.hostname, ep.key, ep.value, ep.source_name ORDER BY d.hostname, ep.key LIMIT 100',
    },
    {
      label: 'Investigations',
      cypher: "MATCH (i:Investigation) RETURN i.device_address, i.detection_id, i.status, i.summary, i.tokens_used, i.started_at ORDER BY i.started_at DESC LIMIT 20",
    },
  ];

  onMount(() => {
    loadSavedQueries();
    loadNlBudget();
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
    insightsError = null;
    try {
      const r = await fetch('/api/graph/insights');
      if (r.ok) {
        insights = await r.json();
      } else {
        insightsError = `Server returned ${r.status}: ${await r.text()}`;
      }
    } catch (e) {
      insightsError = `Failed to load insights: ${e.message}`;
    }
    insightsLoading = false;
  }

  async function loadQuality() {
    qualityLoading = true;
    qualityError = null;
    try {
      const r = await fetch('/api/graph/quality');
      if (r.ok) {
        quality = await r.json();
      } else {
        qualityError = `Server returned ${r.status}: ${await r.text()}`;
      }
    } catch (e) {
      qualityError = `Failed to load graph quality: ${e.message}`;
    }
    qualityLoading = false;
  }

  function switchTab(tab) {
    activeTab = tab;
    if (tab === 'insights' && !insights) loadInsights();
    if (tab === 'health') loadQuality();
    if (tab === 'ask' && !nlBudget) loadNlBudget();
  }

  // ── NL query ────────────────────────────────────────────────────────────────

  async function loadNlBudget() {
    try {
      const r = await fetch('/api/explorer/nl-budget');
      if (r.ok) nlBudget = await r.json();
    } catch {}
  }

  async function askQuestion() {
    if (!nlQuestion.trim() || nlRunning) return;
    nlRunning = true;
    nlError = null;
    nlResult = null;
    try {
      const r = await fetch('/api/explorer/ask', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ question: nlQuestion }),
      });
      if (!r.ok) {
        const msg = await r.text();
        throw new Error(msg);
      }
      nlResult = await r.json();
      nlHistory = [{ question: nlQuestion, result: nlResult }, ...nlHistory].slice(0, 20);
      loadNlBudget(); // refresh budget
    } catch (e) {
      nlError = e.message;
    } finally {
      nlRunning = false;
    }
  }

  function useCypherInExplorer(newCypher) {
    activeTab = 'explorer';
    cypher = newCypher;
    result = null;
    error = null;
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
      <button class="tab-btn" class:active={activeTab === 'ask'} onclick={() => switchTab('ask')}>Ask</button>
      <button class="tab-btn" class:active={activeTab === 'explorer'} onclick={() => switchTab('explorer')}>Cypher</button>
      <button class="tab-btn" class:active={activeTab === 'insights'} onclick={() => switchTab('insights')}>Insights</button>
      <button class="tab-btn" class:active={activeTab === 'health'} onclick={() => switchTab('health')}>Graph Health</button>
    </div>
  </div>

  {#if activeTab === 'ask'}
    <!-- ── NL ask pane ───────────────────────────────────────────────────── -->
    <div class="ask-layout">
      <div class="ask-header">
        <div class="ask-input-wrap">
          <input
            class="ask-input"
            type="text"
            bind:value={nlQuestion}
            placeholder="Ask about your network… e.g. 'Which devices are connected to spine1?' or 'Show me all critical incidents'"
            onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); askQuestion(); } }}
            disabled={nlRunning}
          />
          <button class="btn-primary ask-btn" onclick={askQuestion} disabled={nlRunning || !nlQuestion.trim()}>
            {nlRunning ? 'Thinking…' : 'Ask'}
          </button>
        </div>
        {#if nlBudget}
          <div class="nl-budget">
            <span class="budget-label">Token budget:</span>
            <span class="budget-value {nlBudget.daily_total_tokens > nlBudget.daily_limit * 0.8 ? 'warn' : ''}">
              {nlBudget.daily_total_tokens.toLocaleString()} / {nlBudget.daily_limit.toLocaleString()}
            </span>
          </div>
        {/if}
      </div>

      {#if nlError}
        <div class="error-banner">{nlError}</div>
      {/if}

      {#if nlResult}
        <div class="ask-result-card">
          <div class="ask-question-echo">{nlResult.question}</div>

          {#if nlResult.answer_template && !nlResult.error}
            <div class="ask-answer-summary">{nlResult.answer_template}</div>
          {/if}

          {#if nlResult.explanation}
            <div class="ask-explanation">{nlResult.explanation}</div>
          {/if}

          {#if nlResult.error}
            <div class="error-banner" style="margin-top:0.75rem;">{nlResult.error}</div>
          {:else if nlResult.rows.length > 0}
            <div class="result-meta" style="margin-top:0.75rem;">{nlResult.row_count} row{nlResult.row_count !== 1 ? 's' : ''}</div>
            <div class="result-table-wrap">
              <table class="result-table">
                <thead>
                  <tr>
                    {#each nlResult.columns as col}
                      <th>{col}</th>
                    {/each}
                  </tr>
                </thead>
                <tbody>
                  {#each nlResult.rows as row}
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
            <div class="empty-result" style="margin-top:0.5rem;">Query returned 0 rows — the graph may not have data matching this query yet.</div>
          {/if}

          <details class="ask-cypher-details">
            <summary class="ask-cypher-summary">View generated Cypher <span class="ask-tokens">{nlResult.tokens_used} tokens</span></summary>
            <pre class="ask-cypher-pre">{nlResult.cypher}</pre>
            <button class="btn-secondary ask-use-btn" onclick={() => { cypher = nlResult.cypher; activeTab = 'explorer'; result = null; error = null; }}>
              Open in Cypher editor
            </button>
          </details>
        </div>
      {:else if !nlRunning && !nlError}
        <div class="ask-examples">
          <div class="ask-examples-title">Try asking about your network</div>
          <div class="ask-examples-grid">
            {#each [
              'How many devices per vendor?',
              'Which devices are connected to spine1?',
              'Show me BGP sessions that are down',
              'Are there any unresolved detections?',
              'What interfaces have errors?',
              'Show me recent syslog events',
            ] as example}
              <button class="ask-example-btn" onclick={() => { nlQuestion = example; askQuestion(); }}>
                {example}
              </button>
            {/each}
          </div>
        </div>
      {/if}

      {#if nlHistory.length > 1}
        <div class="ask-history">
          <div class="sidebar-section-title">Recent questions</div>
          {#each nlHistory.slice(1) as item}
            <button class="lib-btn" onclick={() => { nlQuestion = item.question; nlResult = item.result; nlError = null; }}>
              {item.question}
              <span class="ask-history-rows">{item.result.row_count} rows</span>
            </button>
          {/each}
        </div>
      {/if}
    </div>

  {:else if activeTab === 'explorer'}
    <!-- ── explorer pane ─────────────────────────────────────────────────── -->
    <div class="explorer-layout">
      <!-- sidebar -->
      <aside class="query-sidebar">
        <div class="sidebar-section">
          {#each QUERY_LIBRARY as q}
            {#if q.section}
              <div class="sidebar-section-title">{q.section}</div>
            {:else}
              <button class="lib-btn" onclick={() => pickLibraryQuery(q)}>{q.label}</button>
            {/if}
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

  {:else if activeTab === 'health'}
    <!-- ── graph health pane ─────────────────────────────────────────────── -->
    {#if qualityLoading}
      <div class="loading">Computing graph quality…</div>
    {:else if qualityError}
      <div class="empty">
        <div class="empty-title">Could not load graph quality data</div>
        <div class="empty-detail">{qualityError}</div>
        <button class="btn-secondary" style="margin-top:0.75rem;" onclick={() => { quality = null; loadQuality(); }}>Retry</button>
      </div>
    {:else if !quality}
      <div class="empty">
        <div class="empty-title">No graph quality data available</div>
        <div class="empty-detail">The graph database may be empty or not yet populated with device data.</div>
        <button class="btn-secondary" style="margin-top:0.75rem;" onclick={() => loadQuality()}>Retry</button>
      </div>
    {:else}
      <div class="quality-layout">

        <!-- Overall score gauge -->
        <div class="quality-score-card">
          <div class="score-ring" style="--score: {quality.overall_score}">
            <span class="score-val">{quality.overall_score.toFixed(1)}</span>
            <span class="score-label">/ 100</span>
          </div>
          <div class="score-desc">Overall data quality score</div>
        </div>

        <!-- Coverage bars (radar-style list) -->
        <div class="quality-bars-card">
          <div class="card-title">Signal coverage</div>
          {#each [
            { label: 'gNMI subscriptions', cov: quality.gnmi_coverage, weight: '30%' },
            { label: 'Syslog (24 h)', cov: quality.syslog_coverage, weight: '20%' },
            { label: 'Interface counters', cov: quality.interface_counter_coverage, weight: '20%' },
            { label: 'Topology (LLDP)', cov: quality.topology_link_coverage, weight: '15%' },
            { label: 'BGP sessions', cov: quality.bgp_mapped_coverage, weight: '15%' },
            { label: 'BMP sessions', cov: quality.bmp_coverage, weight: '—' },
            { label: 'NetBox enrichment', cov: quality.netbox_enrichment_coverage, weight: '—' },
          ] as dim}
            <div class="cov-row">
              <span class="cov-label">{dim.label}</span>
              <div class="cov-bar-wrap">
                <div
                  class="cov-bar"
                  class:cov-good={dim.cov.pct >= 80}
                  class:cov-warn={dim.cov.pct >= 40 && dim.cov.pct < 80}
                  class:cov-bad={dim.cov.pct < 40}
                  style="width: {Math.min(dim.cov.pct, 100)}%"
                ></div>
              </div>
              <span class="cov-pct {dim.cov.pct >= 80 ? 'good' : dim.cov.pct >= 40 ? 'warn' : 'bad'}">
                {dim.cov.pct.toFixed(1)}%
              </span>
              <span class="cov-detail">{dim.cov.covered}/{dim.cov.total}</span>
              <span class="cov-weight">{dim.weight}</span>
            </div>
          {/each}
        </div>

        <!-- Weak devices -->
        <div class="quality-weak-card">
          <div class="card-title">
            Weak devices
            <span class="card-subtitle">— missing one or more key signals</span>
            <span class="weak-count {quality.weak_devices.length === 0 ? 'healthy' : 'warn'}">
              {quality.weak_devices.length}
            </span>
          </div>
          {#if quality.weak_devices.length === 0}
            <div class="empty-result">All devices have full signal coverage.</div>
          {:else}
            <table class="result-table">
              <thead><tr><th>Address</th><th>Hostname</th><th>Missing signals</th></tr></thead>
              <tbody>
                {#each quality.weak_devices as wd}
                  <tr>
                    <td class="mono">{wd.address}</td>
                    <td>{wd.hostname || '—'}</td>
                    <td>
                      {#each wd.missing as sig}
                        <span class="missing-badge">{sig}</span>
                      {/each}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </div>

      </div>
    {/if}

  {:else}
    <!-- ── insights pane ─────────────────────────────────────────────────── -->
    {#if insightsLoading}
      <div class="loading">Computing graph insights…</div>
    {:else if insightsError}
      <div class="empty">
        <div class="empty-title">Could not load graph insights</div>
        <div class="empty-detail">{insightsError}</div>
        <button class="btn-secondary" style="margin-top:0.75rem;" onclick={() => { insights = null; insightsError = null; loadInsights(); }}>Retry</button>
      </div>
    {:else if !insights}
      <div class="empty">
        <div class="empty-title">No graph insights available</div>
        <div class="empty-detail">The graph database may be empty or not yet populated with device data.</div>
        <button class="btn-secondary" style="margin-top:0.75rem;" onclick={() => loadInsights()}>Retry</button>
      </div>
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
  .empty-title { font-size: 0.95rem; font-weight: 600; color: var(--text, #ccc); margin-bottom: 0.5rem; }
  .empty-detail { font-size: 0.8rem; color: var(--text-muted, #888); max-width: 480px; margin: 0 auto; line-height: 1.5; }

  /* ── ask pane ────────────────────────────────────────────────────────────── */
  .ask-layout { display: flex; flex-direction: column; gap: 1rem; }

  .ask-header { display: flex; flex-direction: column; gap: 0.5rem; }

  .ask-input-wrap {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }

  .ask-input {
    flex: 1;
    padding: 0.6rem 0.85rem;
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    color: var(--text, #e5e5e5);
    font-size: 0.9rem;
    font-family: inherit;
  }
  .ask-input:focus {
    outline: none;
    border-color: var(--accent, #3b82f6);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent, #3b82f6) 25%, transparent);
  }
  .ask-input::placeholder { color: var(--text-muted, #666); }

  .ask-btn { flex-shrink: 0; padding: 0.6rem 1.2rem; font-size: 0.9rem; }

  .nl-budget {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.72rem;
    color: var(--text-muted, #888);
  }
  .budget-label { opacity: 0.7; }
  .budget-value.warn { color: var(--warn, #f59e0b); }

  .ask-result-card {
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    padding: 1rem 1.25rem;
  }

  .ask-question-echo {
    font-size: 0.95rem;
    font-weight: 600;
    margin-bottom: 0.5rem;
    color: var(--text, #e5e5e5);
  }

  .ask-explanation {
    font-size: 0.82rem;
    color: var(--text-muted, #aaa);
    margin-bottom: 0.75rem;
    line-height: 1.5;
  }

  .ask-cypher-details {
    margin-top: 0.25rem;
  }

  .ask-cypher-summary {
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted, #888);
    cursor: pointer;
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }

  .ask-tokens {
    font-size: 0.68rem;
    color: var(--text-muted, #666);
    font-weight: 400;
    text-transform: none;
    letter-spacing: 0;
  }

  .ask-cypher-pre {
    background: var(--bg, #111);
    border: 1px solid var(--border, #333);
    border-radius: 4px;
    padding: 0.6rem 0.75rem;
    font-family: 'JetBrains Mono', 'Fira Code', monospace;
    font-size: 0.78rem;
    line-height: 1.6;
    overflow-x: auto;
    margin: 0.4rem 0;
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--accent, #58a6ff);
  }

  .ask-use-btn {
    font-size: 0.72rem;
    padding: 0.2rem 0.5rem;
    margin-top: 0.25rem;
  }

  .ask-answer-summary {
    font-size: 1rem;
    font-weight: 500;
    color: var(--accent, #58a6ff);
    margin-bottom: 0.75rem;
    padding: 0.6rem 0.85rem;
    background: color-mix(in srgb, var(--accent, #3b82f6) 8%, transparent);
    border-left: 3px solid var(--accent, #3b82f6);
    border-radius: 0 4px 4px 0;
    line-height: 1.5;
  }

  .ask-examples {
    padding: 1.5rem 0;
  }
  .ask-examples-title {
    font-size: 0.9rem;
    color: var(--text-muted, #888);
    margin-bottom: 1rem;
    text-align: center;
  }
  .ask-examples-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
    gap: 0.5rem;
  }
  .ask-example-btn {
    text-align: left;
    padding: 0.6rem 0.85rem;
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    color: var(--text, #ccc);
    cursor: pointer;
    font-size: 0.82rem;
    line-height: 1.4;
    transition: border-color 0.15s, background 0.15s;
  }
  .ask-example-btn:hover {
    border-color: var(--accent, #3b82f6);
    background: color-mix(in srgb, var(--accent, #3b82f6) 5%, var(--surface, #1a1a1a));
  }

  .ask-history {
    margin-top: 0.5rem;
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }

  .ask-history-rows {
    margin-left: auto;
    font-size: 0.7rem;
    color: var(--text-muted, #888);
    font-weight: 400;
  }

  /* ── graph health pane ────────────────────────────────────────────────────── */
  .quality-layout {
    display: grid;
    grid-template-columns: 180px 1fr;
    grid-template-rows: auto 1fr;
    gap: 1rem;
  }

  .quality-score-card {
    grid-row: 1 / 2;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    padding: 1.25rem 1rem;
    gap: 0.5rem;
  }

  .score-ring {
    width: 96px;
    height: 96px;
    border-radius: 50%;
    border: 5px solid var(--border, #333);
    background: conic-gradient(
      var(--accent, #3b82f6) calc(var(--score, 0) * 1%),
      var(--surface2, #252525) 0%
    );
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
  }
  .score-val { font-size: 1.5rem; font-weight: 700; line-height: 1; }
  .score-label { font-size: 0.7rem; color: var(--text-muted, #888); }
  .score-desc { font-size: 0.75rem; color: var(--text-muted, #888); text-align: center; }

  .quality-bars-card {
    grid-row: 1 / 2;
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    padding: 1rem 1.25rem;
  }

  .quality-weak-card {
    grid-column: 1 / 3;
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #333);
    border-radius: 6px;
    padding: 1rem 1.25rem;
  }

  .cov-row {
    display: grid;
    grid-template-columns: 160px 1fr 54px 60px 40px;
    align-items: center;
    gap: 0.6rem;
    padding: 0.3rem 0;
  }
  .cov-label { font-size: 0.82rem; color: var(--text, #ccc); }
  .cov-bar-wrap {
    height: 8px;
    background: var(--surface2, #252525);
    border-radius: 4px;
    overflow: hidden;
  }
  .cov-bar { height: 100%; border-radius: 4px; transition: width 0.4s ease; }
  .cov-bar.cov-good { background: var(--healthy, #22c55e); }
  .cov-bar.cov-warn { background: var(--warning, #f59e0b); }
  .cov-bar.cov-bad  { background: var(--critical, #ef4444); }

  .cov-pct { font-size: 0.82rem; font-weight: 600; font-variant-numeric: tabular-nums; text-align: right; }
  .cov-pct.good { color: var(--healthy, #22c55e); }
  .cov-pct.warn { color: var(--warning, #f59e0b); }
  .cov-pct.bad  { color: var(--critical, #ef4444); }
  .cov-detail { font-size: 0.72rem; color: var(--text-muted, #888); text-align: right; }
  .cov-weight { font-size: 0.68rem; color: var(--text-muted, #888); text-align: right; }

  .weak-count {
    display: inline-block;
    margin-left: 0.5rem;
    padding: 0.1rem 0.45rem;
    border-radius: 10px;
    font-size: 0.75rem;
    font-weight: 600;
  }
  .weak-count.healthy { background: var(--healthy-bg, #14532d); color: var(--healthy, #22c55e); }
  .weak-count.warn    { background: var(--critical-bg, #450a0a); color: var(--critical, #ef4444); }

  .missing-badge {
    display: inline-block;
    margin: 0.1rem 0.2rem 0.1rem 0;
    padding: 0.1rem 0.4rem;
    border-radius: 3px;
    font-size: 0.72rem;
    background: var(--surface2, #252525);
    border: 1px solid var(--border, #333);
    color: var(--warning, #f59e0b);
  }
</style>
