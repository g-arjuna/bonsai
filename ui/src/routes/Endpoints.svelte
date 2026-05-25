<script>
  import { onMount } from 'svelte';
  import { navigate } from '$lib/router.svelte.js';

  let endpoints = $state([]);
  let loading = $state(true);
  let error = $state(null);
  let filter = $state('');
  let kindFilter = $state('all');

  const KIND_COLORS = {
    server:    '#60a5fa',
    vm:        '#a78bfa',
    container: '#34d399',
    endpoint:  '#fbbf24',
    unknown:   '#6b7280',
  };

  function kindColor(kind) {
    return KIND_COLORS[kind?.toLowerCase()] ?? KIND_COLORS.unknown;
  }

  async function load() {
    loading = true;
    error = null;
    try {
      const r = await fetch('/api/endpoints');
      if (!r.ok) throw new Error(await r.text());
      const d = await r.json();
      endpoints = d.endpoints ?? [];
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  onMount(load);

  const allKinds = $derived([...new Set(endpoints.map(e => e.kind || 'unknown'))].sort());

  const filtered = $derived(endpoints.filter(e => {
    if (kindFilter !== 'all' && (e.kind || 'unknown') !== kindFilter) return false;
    if (!filter) return true;
    const q = filter.toLowerCase();
    return (e.ip || '').includes(q)
      || (e.hostname || '').toLowerCase().includes(q)
      || (e.connected_to_device || '').includes(q)
      || (e.vendor || '').toLowerCase().includes(q)
      || (e.source || '').toLowerCase().includes(q);
  }));

  const stats = $derived({
    total: endpoints.length,
    active: endpoints.filter(e => e.recent_flow_count > 0).length,
    kinds: Object.fromEntries(
      [...new Set(endpoints.map(e => e.kind || 'unknown'))].map(k => [
        k, endpoints.filter(e => (e.kind || 'unknown') === k).length
      ])
    ),
  });
</script>

<div class="page">
  <div class="page-header">
    <div>
      <h1>Endpoints</h1>
      <p class="subtitle">HostEndpoint nodes — servers, VMs, containers, and network endpoints discovered via NetBox, LLDP, or NetFlow.</p>
    </div>
    <button class="btn-secondary" onclick={load}>Refresh</button>
  </div>

  {#if !loading && !error}
    <div class="stats-row">
      <div class="stat-card">
        <div class="stat-val">{stats.total}</div>
        <div class="stat-label">Total</div>
      </div>
      <div class="stat-card">
        <div class="stat-val">{stats.active}</div>
        <div class="stat-label">Active flows (60s)</div>
      </div>
      {#each Object.entries(stats.kinds) as [k, n]}
        <div class="stat-card">
          <div class="stat-val" style="color: {kindColor(k)}">{n}</div>
          <div class="stat-label">{k}</div>
        </div>
      {/each}
    </div>
  {/if}

  <div class="toolbar">
    <input class="search-input" type="search" placeholder="Filter by IP, hostname, device, vendor…" bind:value={filter} />
    <select bind:value={kindFilter} class="kind-select">
      <option value="all">All kinds</option>
      {#each allKinds as k}
        <option value={k}>{k}</option>
      {/each}
    </select>
  </div>

  {#if loading}
    <div class="loading">Loading endpoints…</div>
  {:else if error}
    <div class="error">Error: {error}</div>
  {:else if filtered.length === 0}
    <div class="empty">
      {endpoints.length === 0
        ? 'No endpoint nodes discovered yet. Enable NetBox enrichment, LLDP collection, or NetFlow to populate this view.'
        : 'No endpoints match the current filter.'}
    </div>
  {:else}
    <table>
      <thead>
        <tr>
          <th>Host</th>
          <th>IP</th>
          <th>Kind</th>
          <th>MAC</th>
          <th>Vendor</th>
          <th>Connected To</th>
          <th>Interface</th>
          <th>Source</th>
          <th>Flows</th>
        </tr>
      </thead>
      <tbody>
        {#each filtered as e (e.id)}
          <tr>
            <td>
              <span class="hostname">{e.hostname || '—'}</span>
            </td>
            <td><code>{e.ip || '—'}</code></td>
            <td>
              <span class="kind-badge" style="color: {kindColor(e.kind)}; border-color: {kindColor(e.kind)}44">
                {e.kind || 'unknown'}
              </span>
            </td>
            <td><code class="dim">{e.mac || '—'}</code></td>
            <td>{e.vendor || '—'}</td>
            <td>
              {#if e.connected_to_device}
                <button class="link-btn" onclick={() => navigate('/devices/' + encodeURIComponent(e.connected_to_device))}>
                  {e.connected_to_device}
                </button>
              {:else}
                <span class="dim">—</span>
              {/if}
            </td>
            <td><code class="dim">{e.connected_to_iface || '—'}</code></td>
            <td><span class="source-badge">{e.source || '—'}</span></td>
            <td>
              {#if e.recent_flow_count > 0}
                <span class="flow-badge active">{e.recent_flow_count}</span>
              {:else}
                <span class="dim">—</span>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    <div class="row-count">{filtered.length} endpoint{filtered.length !== 1 ? 's' : ''}</div>
  {/if}
</div>

<style>
  .page { padding: 24px 28px; max-width: 1400px; }
  .page-header { display: flex; justify-content: space-between; align-items: flex-start; margin-bottom: 20px; }
  h1 { font-size: 1.4rem; font-weight: 700; margin: 0 0 4px; }
  .subtitle { font-size: 0.82rem; color: var(--color-muted, #6b7280); margin: 0; max-width: 600px; }

  .stats-row { display: flex; gap: 12px; margin-bottom: 18px; flex-wrap: wrap; }
  .stat-card { background: var(--color-surface, #1a1a2e); border: 1px solid var(--color-border, #2d2d44); border-radius: 8px; padding: 12px 18px; min-width: 90px; text-align: center; }
  .stat-val { font-size: 1.5rem; font-weight: 700; }
  .stat-label { font-size: 0.72rem; color: var(--color-muted, #6b7280); text-transform: uppercase; letter-spacing: 0.05em; margin-top: 2px; }

  .toolbar { display: flex; gap: 10px; margin-bottom: 14px; align-items: center; }
  .search-input { flex: 1; padding: 7px 12px; border-radius: 6px; border: 1px solid var(--color-border, #2d2d44); background: var(--color-surface, #1a1a2e); color: inherit; font-size: 0.85rem; }
  .kind-select { padding: 6px 10px; border-radius: 6px; border: 1px solid var(--color-border, #2d2d44); background: var(--color-surface, #1a1a2e); color: inherit; font-size: 0.85rem; }

  table { width: 100%; border-collapse: collapse; font-size: 0.82rem; }
  th { text-align: left; padding: 7px 10px; border-bottom: 1px solid var(--color-border, #2d2d44); color: var(--color-muted, #6b7280); font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.05em; font-weight: 600; white-space: nowrap; }
  td { padding: 7px 10px; border-bottom: 1px solid var(--color-border, #2d2d4422); vertical-align: middle; }
  tr:hover td { background: var(--color-surface, #1a1a2e); }

  .hostname { font-weight: 600; }
  code { font-family: monospace; font-size: 0.8rem; }
  .dim { color: var(--color-muted, #6b7280); }

  .kind-badge { font-size: 0.72rem; font-weight: 600; padding: 2px 7px; border-radius: 4px; border: 1px solid; background: transparent; }
  .source-badge { font-size: 0.72rem; color: var(--color-muted, #6b7280); background: var(--color-border, #2d2d44); padding: 1px 6px; border-radius: 4px; }
  .flow-badge { font-size: 0.72rem; font-weight: 700; padding: 1px 7px; border-radius: 4px; }
  .flow-badge.active { background: #10b98122; color: #10b981; border: 1px solid #10b98144; }

  .link-btn { background: none; border: none; color: #60a5fa; cursor: pointer; font-size: 0.82rem; font-family: monospace; padding: 0; text-decoration: underline; text-underline-offset: 2px; }
  .link-btn:hover { color: #93c5fd; }

  .loading, .empty, .error { padding: 40px; text-align: center; color: var(--color-muted, #6b7280); font-size: 0.88rem; }
  .error { color: #f87171; }

  .row-count { font-size: 0.75rem; color: var(--color-muted, #6b7280); margin-top: 10px; text-align: right; }

  .btn-secondary { background: var(--color-surface, #1a1a2e); border: 1px solid var(--color-border, #2d2d44); border-radius: 6px; padding: 6px 14px; cursor: pointer; color: inherit; font-size: 0.82rem; }
  .btn-secondary:hover { background: var(--color-border, #2d2d44); }
</style>
