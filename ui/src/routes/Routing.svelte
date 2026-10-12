<script>
  import { onMount } from 'svelte';

  let health = $state(null);
  let speakers = $state([]);
  let peers = $state([]);
  let routes = $state([]);
  let coverage = $state([]);
  let prefixHistory = $state([]);
  let routesMeta = $state({
    completeness_state: 'syncing',
    support_status: 'supported',
    durable_sequence: 0,
    receive_time: '',
    total_matching: 0,
    next_cursor: null,
    message: '',
  });

  let loading = $state(true);
  let error = $state(null);

  // Filters for /api/routing/v1/routes
  let selectedSpeaker = $state('');
  let selectedPeer = $state('');
  let selectedView = $state('');
  let selectedVrfRd = $state('');
  let selectedFamily = $state('ipv4-unicast');
  let prefixFilter = $state('');
  let cursor = $state(0);

  // Selected prefix for /api/routing/v1/prefix-history
  let historyPrefix = $state('');
  let loadingHistory = $state(false);

  const RIB_VIEWS = [
    { value: '', label: 'All Views' },
    { value: 'AdjRibInPrePolicy', label: 'Adj-RIB-In (Pre-Policy)' },
    { value: 'AdjRibInPostPolicy', label: 'Adj-RIB-In (Post-Policy)' },
    { value: 'LocRib', label: 'Loc-RIB' },
    { value: 'LocRibFiltered', label: 'Loc-RIB (Filtered)' },
    { value: 'AdjRibOutPrePolicy', label: 'Adj-RIB-Out (Pre-Policy)' },
    { value: 'AdjRibOutPostPolicy', label: 'Adj-RIB-Out (Post-Policy)' },
  ];

  const FAMILIES = [
    { value: 'ipv4-unicast', label: 'IPv4 Unicast (1/1)' },
    { value: 'ipv6-unicast', label: 'IPv6 Unicast (2/1)' },
    { value: 'ipv4-vpn', label: 'VPNv4 L3VPN (1/128)' },
    { value: 'ipv6-vpn', label: 'VPNv6 L3VPN (2/128)' },
    { value: 'l2vpn-evpn', label: 'L2VPN EVPN (25/70)' },
    { value: 'bgp-ls', label: 'BGP-LS (16388/71)' },
  ];

  onMount(() => {
    refreshAll();
    const interval = setInterval(refreshAll, 15_000);
    return () => clearInterval(interval);
  });

  async function refreshAll() {
    loading = true;
    error = null;
    try {
      const [hRes, sRes, pRes, cRes] = await Promise.all([
        fetch('/api/routing/v1/health'),
        fetch('/api/routing/v1/speakers'),
        fetch('/api/routing/v1/peers'),
        fetch('/api/routing/v1/coverage'),
      ]);

      if (!hRes.ok) {
        const body = await hRes.json().catch(() => ({}));
        throw new Error(body.message || `Routing service returned HTTP ${hRes.status}`);
      }

      health = await hRes.json();
      const sData = await sRes.json();
      speakers = sData.speakers || [];
      const pData = await pRes.json();
      peers = pData.peers || [];
      const cData = await cRes.json();
      coverage = cData.families || [];

      await loadRoutes();
      if (historyPrefix.trim()) {
        await loadPrefixHistory(historyPrefix.trim());
      }
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function loadRoutes() {
    try {
      const params = new URLSearchParams();
      if (selectedSpeaker) params.set('speaker', selectedSpeaker);
      if (selectedPeer) params.set('peer', selectedPeer);
      if (selectedView) params.set('view', selectedView);
      if (selectedVrfRd.trim()) params.set('vrf_rd', selectedVrfRd.trim());
      if (selectedFamily) params.set('family', selectedFamily);
      if (prefixFilter.trim()) params.set('prefix', prefixFilter.trim());
      params.set('cursor', String(cursor));
      params.set('limit', '100');

      const r = await fetch(`/api/routing/v1/routes?${params.toString()}`);
      const data = await r.json();
      routes = data.routes || [];
      routesMeta = {
        completeness_state: data.completeness_state || 'syncing',
        support_status: data.support_status || 'supported',
        durable_sequence: data.durable_sequence ?? 0,
        receive_time: data.receive_time || '',
        total_matching: data.total_matching ?? routes.length,
        next_cursor: data.next_cursor ?? null,
        message: data.message || '',
      };
    } catch (e) {
      error = e.message;
    }
  }

  async function loadPrefixHistory(pfx) {
    if (!pfx) return;
    historyPrefix = pfx;
    loadingHistory = true;
    try {
      const params = new URLSearchParams({ prefix: pfx, limit: '50' });
      const r = await fetch(`/api/routing/v1/prefix-history?${params.toString()}`);
      if (r.ok) {
        const data = await r.json();
        prefixHistory = data.events || [];
      }
    } catch (_) {
      prefixHistory = [];
    } finally {
      loadingHistory = false;
    }
  }

  function applyFilters() {
    cursor = 0;
    loadRoutes();
  }

  function completenessBadgeClass(state) {
    switch (state) {
      case 'complete_for_observed_view':
        return 'badge-ok';
      case 'syncing':
        return 'badge-info';
      case 'stale':
        return 'badge-warn';
      case 'gap':
      case 'unsupported':
      case 'unavailable':
        return 'badge-danger';
      default:
        return 'badge-neutral';
    }
  }
</script>

<div class="routing-workspace">
  <header class="workspace-header">
    <div>
      <p class="eyebrow">Canonical BMP / BGP Control Plane</p>
      <h2>Routing &amp; RIB Observatory</h2>
    </div>
    <div class="header-badges">
      {#if health}
        <span class="badge {completenessBadgeClass(health.completeness_state)}" title="Overall RIB Completeness State">
          Completeness: {health.completeness_state}
        </span>
        <span class="badge badge-neutral" title="Durable RibEvent Journal Sequence">
          Seq #{health.durable_sequence ?? 0}
        </span>
        {#if health.graph_projection_lag !== undefined}
          <span class="badge {health.graph_projection_lag > 0 ? 'badge-warn' : 'badge-ok'}" title="Graph Summary Projection Lag">
            Graph Lag: {health.graph_projection_lag}
          </span>
        {/if}
        <span class="badge badge-neutral" title="Last Routing Health Snapshot Time">
          Observed: {health.receive_time ? new Date(health.receive_time).toLocaleTimeString() : '—'}
        </span>
      {/if}
      <button class="btn-refresh" onclick={refreshAll}>Refresh</button>
    </div>
  </header>

  {#if error}
    <div class="alert alert-danger" role="alert">
      <strong>Routing Service Unavailable or Degraded:</strong> {error}
    </div>
  {/if}

  {#if routesMeta.completeness_state !== 'complete_for_observed_view'}
    <div class="alert alert-warn" role="status">
      <strong>Data-Quality Notice ({routesMeta.completeness_state}):</strong>
      {#if routesMeta.completeness_state === 'unsupported'}
        {routesMeta.message || `Address family '${selectedFamily}' has support status '${routesMeta.support_status}' and is not projected as a complete RIB view.`}
      {:else if routesMeta.completeness_state === 'gap'}
        One or more BMP speaker sessions experienced a transport disconnect without explicit BGP route withdrawals. Last-known routes are preserved with a transport-gap indicator.
      {:else if routesMeta.completeness_state === 'stale'}
        Long-Lived Graceful Restart (LLGR) stale routes are present while a session is recovering.
      {:else}
        Initial table dump or End-of-RIB marker has not completed for all observed views (`syncing`). An empty or partial table does not imply absence of routes on the router.
      {/if}
    </div>
  {/if}

  <!-- Summary KPI Row -->
  <section class="kpi-grid" aria-label="Routing Summary">
    <div class="kpi-card">
      <span class="kpi-label">BMP Speakers</span>
      <strong class="kpi-value">{health?.speakers_connected ?? speakers.length}</strong>
    </div>
    <div class="kpi-card">
      <span class="kpi-label">BGP Peers Up</span>
      <strong class="kpi-value">{health?.peers_up ?? 0}</strong>
    </div>
    <div class="kpi-card">
      <span class="kpi-label">Active RIB Routes</span>
      <strong class="kpi-value">{health?.total_routes ?? 0}</strong>
    </div>
    <div class="kpi-card">
      <span class="kpi-label">Transport Gaps</span>
      <strong class="kpi-value">{health?.transport_gaps ?? 0}</strong>
    </div>
  </section>

  <!-- Speakers & Peer Sessions -->
  <section class="panel-grid">
    <div class="panel">
      <h3>BMP Exporters (Speakers)</h3>
      {#if speakers.length === 0}
        <p class="empty-hint">No BMP speakers connected yet.</p>
      {:else}
        <table class="data-table">
          <thead>
            <tr>
              <th>Speaker IP</th>
              <th>SysName</th>
              <th>Epoch</th>
              <th>Peers (Up/Total)</th>
              <th>Routes</th>
              <th>Completeness</th>
            </tr>
          </thead>
          <tbody>
            {#each speakers as spk}
              <tr>
                <td><code>{spk.speaker_addr}</code></td>
                <td>{spk.sys_name || '—'}</td>
                <td>{spk.source?.session_epoch ?? 0}</td>
                <td>{spk.up_peer_count}/{spk.peer_count}</td>
                <td>{spk.total_routes}</td>
                <td>
                  <span class="badge {completenessBadgeClass(spk.completeness_state)}">
                    {spk.completeness_state}
                  </span>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>

    <div class="panel">
      <h3>BGP Peer / Instance Sessions</h3>
      {#if peers.length === 0}
        <p class="empty-hint">No BGP peer sessions observed.</p>
      {:else}
        <table class="data-table">
          <thead>
            <tr>
              <th>Speaker</th>
              <th>Peer IP</th>
              <th>ASN</th>
              <th>VRF / RD</th>
              <th>State</th>
              <th>EoR Families</th>
              <th>Completeness</th>
            </tr>
          </thead>
          <tbody>
            {#each peers as peer}
              <tr>
                <td><code>{peer.speaker_addr}</code></td>
                <td><code>{peer.peer_address}</code></td>
                <td>AS{peer.peer_as}</td>
                <td><code>{peer.peer_distinguisher}</code></td>
                <td>
                  <span class="badge {peer.state === 'up' ? 'badge-ok' : 'badge-danger'}">
                    {peer.state}
                  </span>
                </td>
                <td>{(peer.eor_families || []).join(', ') || 'None'}</td>
                <td>
                  <span class="badge {completenessBadgeClass(peer.completeness_state)}">
                    {peer.completeness_state}
                  </span>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  </section>

  <!-- RIB Explorer & Filters -->
  <section class="panel">
    <div class="panel-header">
      <h3>Canonical RIB Explorer</h3>
      <div class="filter-bar">
        <select bind:value={selectedFamily} onchange={applyFilters} aria-label="Address Family">
          {#each FAMILIES as fam}
            <option value={fam.value}>{fam.label}</option>
          {/each}
        </select>

        <select bind:value={selectedView} onchange={applyFilters} aria-label="RIB View">
          {#each RIB_VIEWS as v}
            <option value={v.value}>{v.label}</option>
          {/each}
        </select>

        <input
          type="text"
          placeholder="Speaker IP"
          bind:value={selectedSpeaker}
          aria-label="Speaker IP Filter"
        />
        <input
          type="text"
          placeholder="Peer IP"
          bind:value={selectedPeer}
          aria-label="Peer IP Filter"
        />
        <input
          type="text"
          placeholder="VRF RD (e.g. 0:0 or 65000:100)"
          bind:value={selectedVrfRd}
          aria-label="VRF RD Filter"
        />
        <input
          type="text"
          placeholder="Prefix (e.g. 10.10.0.0/24)"
          bind:value={prefixFilter}
          aria-label="Prefix Filter"
        />
        <button class="btn-primary" onclick={applyFilters}>Filter Routes</button>
      </div>
    </div>

    {#if routesMeta.support_status !== 'supported'}
      <div class="empty-hint">
        Address family <code>{selectedFamily}</code> has status
        <span class="badge badge-danger">{routesMeta.support_status}</span>:
        {routesMeta.message}
      </div>
    {:else if routes.length === 0}
      <p class="empty-hint">
        No routes match current filter (Completeness: <code>{routesMeta.completeness_state}</code>).
      </p>
    {:else}
      <table class="data-table">
        <thead>
          <tr>
            <th>Prefix</th>
            <th>View</th>
            <th>VRF / RD</th>
            <th>Speaker</th>
            <th>Peer (ASN)</th>
            <th>Next Hop</th>
            <th>AS Path</th>
            <th>LP / MED</th>
            <th>Communities</th>
            <th>State / Freshness</th>
            <th>History</th>
          </tr>
        </thead>
        <tbody>
          {#each routes as rt}
            <tr>
              <td><code>{rt.prefix}</code></td>
              <td><code>{rt.view}</code></td>
              <td><code>{rt.peer_distinguisher}</code></td>
              <td><code>{rt.speaker_addr}</code></td>
              <td><code>{rt.peer_addr}</code> (AS{rt.peer_as})</td>
              <td><code>{rt.next_hop || '—'}</code></td>
              <td><code>{rt.as_path || 'i'}</code></td>
              <td>{rt.local_pref ?? '—'} / {rt.med ?? '—'}</td>
              <td><code>{(rt.communities || []).join(' ') || '—'}</code></td>
              <td>
                <span class="badge {completenessBadgeClass(rt.completeness_state)}">
                  {rt.completeness_state}
                </span>
              </td>
              <td>
                <button class="btn-small" onclick={() => loadPrefixHistory(rt.prefix)}>
                  Timeline
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <!-- Prefix History Timeline (Before / After Path Attributes) -->
  <section class="panel">
    <div class="panel-header">
      <h3>Prefix Event History Timeline</h3>
      <div class="filter-bar">
        <input
          type="text"
          placeholder="Inspect prefix history (e.g. 10.10.0.0/24)"
          bind:value={historyPrefix}
          aria-label="Prefix History Query"
        />
        <button class="btn-primary" onclick={() => loadPrefixHistory(historyPrefix.trim())}>
          Load Timeline
        </button>
      </div>
    </div>

    {#if loadingHistory}
      <p class="empty-hint">Loading prefix history...</p>
    {:else if prefixHistory.length === 0}
      <p class="empty-hint">Select a route or enter a prefix above to inspect announce/withdraw transitions and before/after attributes.</p>
    {:else}
      <table class="data-table">
        <thead>
          <tr>
            <th>Seq #</th>
            <th>Observed At</th>
            <th>Action</th>
            <th>View</th>
            <th>Speaker / Peer</th>
            <th>Previous Path Attributes</th>
            <th>Current Path Attributes</th>
          </tr>
        </thead>
        <tbody>
          {#each prefixHistory as ev}
            <tr>
              <td><code>#{ev.sequence}</code></td>
              <td>{ev.occurred_at ? new Date(ev.occurred_at).toLocaleString() : '—'}</td>
              <td>
                <span class="badge {ev.action === 'Withdraw' ? 'badge-danger' : 'badge-ok'}">
                  {ev.action}
                </span>
              </td>
              <td><code>{ev.view?.rib_type || '—'}</code></td>
              <td><code>{ev.speaker}</code> / <code>{ev.peer}</code> (AS{ev.peer_as})</td>
              <td>
                {#if ev.previous_attributes}
                  <code>
                    NH: {ev.previous_attributes.next_hop || '—'} |
                    AS: {ev.previous_attributes.as_path || '—'} |
                    LP: {ev.previous_attributes.local_pref ?? '—'}
                  </code>
                {:else}
                  <span class="muted">None (initial)</span>
                {/if}
              </td>
              <td>
                {#if ev.attributes}
                  <code>
                    NH: {ev.attributes.next_hop || '—'} |
                    AS: {ev.attributes.as_path || '—'} |
                    LP: {ev.attributes.local_pref ?? '—'}
                  </code>
                {:else}
                  <span class="muted">Withdrawn</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <!-- Address-Family Coverage Matrix -->
  <section class="panel">
    <h3>AFI / SAFI Protocol Coverage Matrix</h3>
    <div class="coverage-row">
      {#each coverage as fam}
        <div class="coverage-chip">
          <strong>{fam.family}</strong>
          <span>({fam.afi}/{fam.safi})</span>
          <span class="badge {fam.support_status === 'supported' ? 'badge-ok' : 'badge-warn'}">
            {fam.support_status}
          </span>
        </div>
      {/each}
    </div>
  </section>
</div>

<style>
  .routing-workspace {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    padding: 1rem 1.25rem;
  }
  .workspace-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.75rem;
  }
  .header-badges {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .kpi-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
    gap: 0.75rem;
  }
  .kpi-card {
    background: var(--surface-2, #161b22);
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    padding: 0.75rem 1rem;
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .kpi-label {
    font-size: 0.75rem;
    color: var(--text-muted, #8b949e);
    text-transform: uppercase;
  }
  .kpi-value {
    font-size: 1.4rem;
  }
  .panel-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(420px, 1fr));
    gap: 1rem;
  }
  .panel {
    background: var(--surface-2, #161b22);
    border: 1px solid var(--border, #30363d);
    border-radius: 6px;
    padding: 1rem;
    overflow-x: auto;
  }
  .panel-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.75rem;
    margin-bottom: 0.75rem;
  }
  .filter-bar {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
    align-items: center;
  }
  .filter-bar input,
  .filter-bar select {
    background: var(--surface-1, #0d1117);
    color: var(--text, #e6edf3);
    border: 1px solid var(--border, #30363d);
    border-radius: 4px;
    padding: 0.35rem 0.6rem;
    font-size: 0.85rem;
  }
  .data-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.85rem;
  }
  .data-table th,
  .data-table td {
    text-align: left;
    padding: 0.45rem 0.6rem;
    border-bottom: 1px solid var(--border, #21262d);
  }
  .badge {
    display: inline-block;
    padding: 0.15rem 0.45rem;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 600;
  }
  .badge-ok {
    background: rgba(46, 160, 67, 0.2);
    color: #3fb950;
  }
  .badge-info {
    background: rgba(56, 139, 253, 0.2);
    color: #58a6ff;
  }
  .badge-warn {
    background: rgba(210, 153, 34, 0.2);
    color: #d29922;
  }
  .badge-danger {
    background: rgba(248, 81, 73, 0.2);
    color: #f85149;
  }
  .badge-neutral {
    background: rgba(110, 118, 129, 0.2);
    color: #8b949e;
  }
  .alert {
    padding: 0.65rem 0.9rem;
    border-radius: 6px;
    font-size: 0.85rem;
  }
  .alert-warn {
    background: rgba(210, 153, 34, 0.15);
    border: 1px solid rgba(210, 153, 34, 0.4);
  }
  .alert-danger {
    background: rgba(248, 81, 73, 0.15);
    border: 1px solid rgba(248, 81, 73, 0.4);
  }
  .coverage-row {
    display: flex;
    gap: 0.75rem;
    flex-wrap: wrap;
    margin-top: 0.5rem;
  }
  .coverage-chip {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.35rem 0.65rem;
    background: var(--surface-1, #0d1117);
    border: 1px solid var(--border, #30363d);
    border-radius: 4px;
    font-size: 0.8rem;
  }
  .btn-primary,
  .btn-refresh,
  .btn-small {
    cursor: pointer;
    border-radius: 4px;
    border: 1px solid var(--border, #30363d);
    background: var(--surface-3, #21262d);
    color: var(--text, #e6edf3);
    padding: 0.35rem 0.7rem;
    font-size: 0.8rem;
  }
  .empty-hint,
  .muted {
    color: var(--text-muted, #8b949e);
    font-size: 0.85rem;
  }
</style>

