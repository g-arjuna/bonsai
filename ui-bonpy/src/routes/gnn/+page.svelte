<script>
  import { createQuery } from '@tanstack/svelte-query';
  import { api } from '$lib/api.js';
  import { lastGnnEvent } from '$lib/sse.js';

  const gnnQ = createQuery({ queryKey: ['gnnResults'], queryFn: api.gnn.results, refetchInterval: 30000 });

  let selectedDevice = null;

  function scoreColor(s) {
    if (s > 0.8) return '#f85149';
    if (s > 0.5) return '#d29922';
    return '#3fb950';
  }
  function fmt(ns) { return ns ? new Date(ns / 1e6).toLocaleTimeString() : '—'; }

  $: rawResults = $gnnQ.data?.results ?? $gnnQ.data ?? [];
  $: results = rawResults.map(r => ({
    ...r,
    top_neighbour: r.top_neighbour ?? r.top_contributing_device,
    top_attention: r.top_attention ?? r.attention_weight,
  }));
  $: anomalous = results.filter(r => r.is_anomalous);
  $: selectedResult = results.find(r => r.device_address === selectedDevice);
  $: gnnLive = $lastGnnEvent?.payload;
</script>

<div class="page">
  <h1 class="page-title">GNN Inference</h1>

  {#if gnnLive}
  <div class="live-banner">
    <span class="dot pulse"></span>
    Live · {gnnLive.anomalous_count ?? 0} anomalous of {gnnLive.total_devices ?? '—'} devices
    · Model {gnnLive.model_id ?? '—'}
    · {fmt(gnnLive.inference_at_ns)}
  </div>
  {/if}

  <div class="two-col">
    <section class="section">
      <h2 class="section-title">Latest Inference Results</h2>
      {#if $gnnQ.isLoading}<p class="muted">Loading…</p>
      {:else}
      <table class="table">
        <thead><tr><th>Device</th><th>Score</th><th>Anomalous</th><th>Top Neighbour</th></tr></thead>
        <tbody>
          {#each results as r}
          <tr class="clickable {selectedDevice === r.device_address ? 'selected' : ''}"
              on:click={() => selectedDevice = selectedDevice === r.device_address ? null : r.device_address}>
            <td class="mono text-sm">{r.device_address}</td>
            <td>
              <div class="score-bar-wrap">
                <div class="score-bar" style="width: {(r.anomaly_score || 0) * 100}%; background: {scoreColor(r.anomaly_score)};"></div>
              </div>
              <span class="score-label" style="color:{scoreColor(r.anomaly_score)}">{r.anomaly_score?.toFixed(3) ?? '—'}</span>
            </td>
            <td>{#if r.is_anomalous}<span class="badge red">YES</span>{:else}<span class="badge gray">no</span>{/if}</td>
            <td class="mono text-sm muted">{r.top_neighbour ?? '—'} ({r.top_attention?.toFixed(2) ?? '—'})</td>
          </tr>
          {/each}
        </tbody>
      </table>
      {/if}
    </section>

    {#if selectedResult}
    <section class="section attention-panel">
      <h2 class="section-title">Attention — {selectedResult.device_address}</h2>
      <div class="attn-score">Anomaly score: <strong style="color:{scoreColor(selectedResult.anomaly_score)}">{selectedResult.anomaly_score?.toFixed(4)}</strong></div>
      {#if selectedResult.neighbours?.length > 0}
      <ul class="attn-list">
        {#each selectedResult.neighbours.slice(0,5) as n}
        <li>
          <span class="mono text-sm">{n.address}</span>
          <div class="attn-bar-wrap">
            <div class="attn-bar" style="width: {n.weight * 100}%"></div>
          </div>
          <span class="muted text-sm">{n.weight?.toFixed(3)}</span>
        </li>
        {/each}
      </ul>
      {:else}
      <p class="muted">No neighbour attention data</p>
      {/if}
      {#if selectedResult.investigation_id}
      <a href="/investigations/{selectedResult.investigation_id}" class="inv-link">View Investigation →</a>
      {/if}
    </section>
    {/if}
  </div>
</div>

<style>
  .page-title { font-size: 20px; font-weight: 700; margin: 0 0 16px; }
  .live-banner { display: flex; align-items: center; gap: 8px; background: rgba(79,142,247,0.08); border: 1px solid rgba(79,142,247,0.2); border-radius: 6px; padding: 8px 14px; font-size: 13px; margin-bottom: 16px; }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: #3fb950; }
  .dot.pulse { animation: pulse 1.5s infinite; }
  @keyframes pulse { 0%,100% { opacity:1; } 50% { opacity:0.4; } }
  .two-col { display: grid; grid-template-columns: 1fr 320px; gap: 20px; }
  .section { margin-bottom: 24px; }
  .section-title { font-size: 13px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-secondary, #8b949e); margin: 0 0 10px; }
  .table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .table th { text-align: left; padding: 6px 8px; border-bottom: 1px solid var(--border, #30363d); color: var(--text-secondary, #8b949e); font-size: 11px; text-transform: uppercase; }
  .table td { padding: 6px 8px; border-bottom: 1px solid var(--border, #30363d); }
  .clickable { cursor: pointer; }
  .clickable:hover td { background: var(--bg-hover, #21262d); }
  .selected td { background: rgba(79,142,247,0.06); }
  .mono { font-family: 'JetBrains Mono', monospace; }
  .muted { color: var(--text-secondary, #8b949e); }
  .text-sm { font-size: 12px; }
  .score-bar-wrap { display: inline-block; width: 60px; height: 4px; background: var(--bg-hover, #21262d); border-radius: 2px; vertical-align: middle; margin-right: 6px; overflow: hidden; }
  .score-bar { height: 100%; border-radius: 2px; transition: width 0.3s; }
  .score-label { font-family: 'JetBrains Mono', monospace; font-size: 11px; }
  .badge { padding: 2px 6px; border-radius: 4px; font-size: 11px; font-weight: 600; }
  .badge.red  { background: rgba(248,81,73,0.15); color: #f85149; }
  .badge.gray { background: rgba(139,148,158,0.1); color: #8b949e; }
  .attention-panel { background: var(--bg-surface, #161b22); border: 1px solid var(--border, #30363d); border-radius: 8px; padding: 16px; }
  .attn-score { font-size: 13px; margin-bottom: 12px; }
  .attn-list { list-style: none; margin: 0; padding: 0; }
  .attn-list li { display: grid; grid-template-columns: 160px 1fr 50px; align-items: center; gap: 8px; padding: 5px 0; border-bottom: 1px solid var(--border, #30363d); }
  .attn-bar-wrap { height: 4px; background: var(--bg-hover, #21262d); border-radius: 2px; overflow: hidden; }
  .attn-bar { height: 100%; background: var(--accent-primary, #4f8ef7); }
  .inv-link { display: inline-block; margin-top: 12px; font-size: 12px; color: var(--accent-primary, #4f8ef7); text-decoration: none; }
</style>
