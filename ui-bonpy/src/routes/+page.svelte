<script>
  import { createQuery } from '@tanstack/svelte-query';
  import { api } from '$lib/api.js';
  import { lastGnnEvent, lastJobEvent } from '$lib/sse.js';

  const sidecarQ  = createQuery({ queryKey: ['sidecar'],  queryFn: api.sidecar.status });
  const activeQ   = createQuery({ queryKey: ['activeModel'], queryFn: () => api.models.active('stgnn') });
  const exportsQ  = createQuery({ queryKey: ['exportsQuality'], queryFn: api.exports.quality });
  const schedulesQ = createQuery({ queryKey: ['schedules'], queryFn: api.schedules.list });

  function fmt(ns) {
    if (!ns) return '—';
    const d = new Date(ns / 1e6);
    const diff = Date.now() - d;
    if (diff < 60000) return `${Math.round(diff/1000)}s ago`;
    if (diff < 3600000) return `${Math.round(diff/60000)}m ago`;
    return `${Math.round(diff/3600000)}h ago`;
  }

  $: gnnEvent = $lastGnnEvent;
  $: jobEvent = $lastJobEvent;

  $: nextJobs = ($schedulesQ.data || [])
    .filter(s => s.enabled && s.next_run_at)
    .sort((a, b) => a.next_run_at - b.next_run_at)
    .slice(0, 5);
</script>

<div class="page">
  <h1 class="page-title">Dashboard</h1>

  <div class="health-strip">
    {#each [
      { label: 'Sidecar',    ok: $sidecarQ.data?.healthy,   val: $sidecarQ.data ? 'healthy' : '—' },
      { label: 'Model',      ok: $activeQ.data?.id,         val: $activeQ.data?.version || '—' },
      { label: 'Last GNN',   ok: !!gnnEvent,                val: gnnEvent ? fmt(gnnEvent.payload?.inference_at_ns) : '—' },
      { label: 'Last Export',ok: $exportsQ.data?.length > 0,val: $exportsQ.data?.[0] ? fmt($exportsQ.data[0].last_export_at) : '—' },
    ] as item}
      <div class="health-chip {item.ok ? 'ok' : 'warn'}">
        <span class="dot"></span>
        <span class="hlabel">{item.label}</span>
        <span class="hval">{item.val}</span>
      </div>
    {/each}
  </div>

  <div class="card-grid">
    <div class="card">
      <div class="card-title">Active Model</div>
      {#if $activeQ.isLoading}<span class="muted">Loading…</span>
      {:else if $activeQ.data}
        <div class="big-val">{$activeQ.data.version || $activeQ.data.model_type}</div>
        <div class="meta">AUC {$activeQ.data.val_auc?.toFixed(3) ?? '—'} · F1 {$activeQ.data.val_f1?.toFixed(3) ?? '—'}</div>
        <div class="meta">Threshold {$activeQ.data.threshold ?? '—'} · Activated {fmt($activeQ.data.trained_at_ns)}</div>
      {:else}<span class="muted">No active model</span>{/if}
    </div>

    <div class="card">
      <div class="card-title">Parquet Freshness</div>
      {#if $exportsQ.isLoading}<span class="muted">Loading…</span>
      {:else if $exportsQ.data?.length > 0}
        {@const ex = $exportsQ.data[0]}
        <div class="big-val">{ex.row_count?.toLocaleString() ?? '—'} rows</div>
        <div class="meta">Last export {fmt(ex.last_export_at)}</div>
        <div class="badge {ex.quality_passed ? 'pass' : 'fail'}">{ex.quality_passed ? 'PASS' : 'FAIL'}</div>
        <div class="meta">Class balance {ex.class_balance_pct?.toFixed(1) ?? '—'}%</div>
      {:else}<span class="muted">No exports yet</span>{/if}
    </div>

    <div class="card">
      <div class="card-title">GNN Status</div>
      {#if gnnEvent}
        <div class="big-val">{gnnEvent.payload?.anomalous_count ?? 0} anomalous</div>
        <div class="meta">of {gnnEvent.payload?.total_devices ?? '—'} devices</div>
        <div class="meta">Top: {gnnEvent.payload?.top_device ?? '—'} ({gnnEvent.payload?.top_score?.toFixed(3) ?? '—'})</div>
      {:else}<span class="muted">No inference yet</span>{/if}
    </div>

    <div class="card">
      <div class="card-title">Upcoming Jobs</div>
      {#if nextJobs.length > 0}
        <ul class="job-list">
          {#each nextJobs as j}
            <li>
              <span class="jid">{j.job_id}</span>
              <span class="muted">{j.cron_expr}</span>
            </li>
          {/each}
        </ul>
      {:else}<span class="muted">No schedules loaded</span>{/if}
    </div>
  </div>
</div>

<style>
  .page-title { font-size: 20px; font-weight: 700; margin: 0 0 20px; }
  .health-strip { display: flex; gap: 8px; flex-wrap: wrap; margin-bottom: 20px; }
  .health-chip { display: flex; align-items: center; gap: 6px; background: var(--bg-surface, #161b22); border: 1px solid var(--border, #30363d); border-radius: 6px; padding: 6px 12px; font-size: 12px; }
  .health-chip.ok .dot { background: #3fb950; }
  .health-chip.warn .dot { background: #d29922; }
  .dot { width: 7px; height: 7px; border-radius: 50%; }
  .hlabel { color: var(--text-secondary, #8b949e); }
  .hval { color: var(--text-primary, #e6edf3); font-weight: 500; }
  .card-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(280px, 1fr)); gap: 16px; }
  .card { background: var(--bg-surface, #161b22); border: 1px solid var(--border, #30363d); border-radius: 8px; padding: 16px; }
  .card-title { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-secondary, #8b949e); margin-bottom: 10px; }
  .big-val { font-size: 22px; font-weight: 700; }
  .meta { font-size: 12px; color: var(--text-secondary, #8b949e); margin-top: 4px; }
  .muted { color: var(--text-secondary, #8b949e); font-size: 13px; }
  .badge { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; margin-top: 6px; }
  .badge.pass { background: rgba(63,185,80,0.15); color: #3fb950; }
  .badge.fail { background: rgba(248,81,73,0.15); color: #f85149; }
  .job-list { list-style: none; margin: 0; padding: 0; font-size: 12px; }
  .job-list li { display: flex; justify-content: space-between; padding: 4px 0; border-bottom: 1px solid var(--border, #30363d); }
  .jid { font-family: 'JetBrains Mono', monospace; }
</style>
