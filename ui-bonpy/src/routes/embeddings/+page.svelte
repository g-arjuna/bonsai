<script>
  import { createQuery } from '@tanstack/svelte-query';
  import { api } from '$lib/api.js';

  const statsQ    = createQuery({ queryKey: ['embeddingStats'],  queryFn: api.embeddings.stats, refetchInterval: 30000 });
  const clustersQ = createQuery({ queryKey: ['syslogClusters'], queryFn: api.syslogClusters });

  let selectedCluster = null;

  $: stats = $statsQ.data;
  $: clusters = ($clustersQ.data?.clusters ?? $clustersQ.data ?? []).sort((a, b) => b.event_count - a.event_count);

  function pct(a, b) {
    if (!a || !b) return 0;
    return Math.round((a / (a + b)) * 100);
  }
</script>

<div class="page">
  <h1 class="page-title">Embeddings</h1>

  <div class="health-grid">
    {#each [
      { label: 'Syslog embedded', val: stats?.syslog_embedded, pending: stats?.syslog_pending, type: 'syslog' },
      { label: 'Config embedded', val: stats?.config_embedded, pending: stats?.config_pending, type: 'config' },
    ] as card}
    <div class="health-card">
      <div class="card-title">{card.label}</div>
      {#if $statsQ.isLoading}<span class="muted">Loading…</span>
      {:else}
        <div class="big-val">{card.val?.toLocaleString() ?? '—'}</div>
        <div class="progress-wrap">
          <div class="progress-fill" style="width: {pct(card.val, card.pending)}%"></div>
        </div>
        <div class="meta">{card.pending?.toLocaleString() ?? '—'} pending · model: {stats?.model_name ?? '—'}</div>
        {#if stats?.last_embed_at}
        <div class="meta">Last batch: {new Date(stats.last_embed_at / 1e6).toLocaleTimeString()}</div>
        {/if}
      {/if}
    </div>
    {/each}
  </div>

  <section class="section">
    <h2 class="section-title">Syslog Cluster Explorer</h2>
    <div class="cluster-layout">
      <div class="cluster-list">
        {#if $clustersQ.isLoading}<p class="muted">Loading…</p>
        {:else if clusters.length === 0}<p class="muted">No clusters yet</p>
        {:else}
          {#each clusters as c}
          <div class="cluster-row {selectedCluster?.id === c.id ? 'active' : ''}"
               on:click={() => selectedCluster = selectedCluster?.id === c.id ? null : c}>
            <div class="cluster-id">C{c.id}</div>
            <div class="cluster-info">
              <div class="cluster-label">{c.label || '(unlabelled)'}</div>
              <div class="cluster-meta">{c.event_count} events</div>
            </div>
            <div class="cluster-bar-wrap">
              <div class="cluster-bar" style="width: {Math.min(100, (c.event_count / (clusters[0]?.event_count || 1)) * 100)}%"></div>
            </div>
          </div>
          {/each}
        {/if}
      </div>

      {#if selectedCluster}
      <div class="cluster-detail">
        <div class="cd-title">Cluster {selectedCluster.id} — {selectedCluster.label || 'unlabelled'}</div>
        <div class="cd-count">{selectedCluster.event_count} events</div>
        {#if selectedCluster.top_event_types}
        <div class="cd-section">Top event types</div>
        <ul class="cd-list">
          {#each (selectedCluster.top_event_types || []).slice(0, 6) as t}
          <li class="mono text-sm">{t}</li>
          {/each}
        </ul>
        {/if}
      </div>
      {/if}
    </div>
  </section>
</div>

<style>
  .page-title { font-size: 20px; font-weight: 700; margin: 0 0 20px; }
  .health-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); gap: 14px; margin-bottom: 24px; }
  .health-card { background: var(--bg-surface, #161b22); border: 1px solid var(--border, #30363d); border-radius: 8px; padding: 16px; }
  .card-title { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-secondary, #8b949e); margin-bottom: 8px; }
  .big-val { font-size: 26px; font-weight: 700; margin-bottom: 8px; }
  .progress-wrap { height: 4px; background: var(--bg-hover, #21262d); border-radius: 2px; overflow: hidden; margin-bottom: 6px; }
  .progress-fill { height: 100%; background: var(--accent-primary, #4f8ef7); border-radius: 2px; }
  .meta { font-size: 12px; color: var(--text-secondary, #8b949e); margin-top: 2px; }
  .section { margin-bottom: 24px; }
  .section-title { font-size: 13px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-secondary, #8b949e); margin: 0 0 10px; }
  .muted { color: var(--text-secondary, #8b949e); font-size: 13px; }
  .cluster-layout { display: grid; grid-template-columns: 1fr 280px; gap: 16px; }
  .cluster-list { display: flex; flex-direction: column; gap: 2px; }
  .cluster-row { display: grid; grid-template-columns: 36px 1fr 100px; align-items: center; gap: 10px; padding: 8px 10px; border-radius: 6px; cursor: pointer; }
  .cluster-row:hover { background: var(--bg-hover, #21262d); }
  .cluster-row.active { background: rgba(79,142,247,0.08); border: 1px solid rgba(79,142,247,0.2); }
  .cluster-id { font-family: 'JetBrains Mono', monospace; font-size: 11px; font-weight: 700; color: var(--accent-primary, #4f8ef7); }
  .cluster-label { font-size: 13px; font-weight: 500; }
  .cluster-meta { font-size: 11px; color: var(--text-secondary, #8b949e); }
  .cluster-bar-wrap { height: 4px; background: var(--bg-hover, #21262d); border-radius: 2px; overflow: hidden; }
  .cluster-bar { height: 100%; background: var(--accent-primary, #4f8ef7); }
  .cluster-detail { background: var(--bg-surface, #161b22); border: 1px solid var(--border, #30363d); border-radius: 8px; padding: 16px; align-self: start; }
  .cd-title { font-weight: 600; margin-bottom: 4px; }
  .cd-count { font-size: 12px; color: var(--text-secondary, #8b949e); margin-bottom: 12px; }
  .cd-section { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-secondary, #8b949e); margin-bottom: 6px; }
  .cd-list { list-style: none; margin: 0; padding: 0; font-size: 12px; }
  .cd-list li { padding: 3px 0; border-bottom: 1px solid var(--border, #30363d); }
  .mono { font-family: 'JetBrains Mono', monospace; }
  .text-sm { font-size: 12px; }
</style>
