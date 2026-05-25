<script>
  import { createQuery, createMutation, useQueryClient } from '@tanstack/svelte-query';
  import { api } from '$lib/api.js';

  const qc = useQueryClient();
  const exportsQ  = createQuery({ queryKey: ['exports'],  queryFn: api.exports.list });
  const qualityQ  = createQuery({ queryKey: ['exportsQuality'], queryFn: api.exports.quality });
  const schedQ    = createQuery({ queryKey: ['schedules'], queryFn: api.schedules.list });

  let detailExport = null;

  function fmt(ns) {
    if (!ns) return '—';
    return new Date(ns / 1e6).toLocaleString();
  }
  function age(ns) {
    if (!ns) return '—';
    const h = (Date.now() - ns / 1e6) / 3600000;
    return h < 1 ? `${Math.round(h*60)}m` : `${h.toFixed(1)}h`;
  }
  function qualityColor(q) {
    if (q === null || q === undefined) return 'gray';
    return q ? 'green' : 'red';
  }

  $: exports = $exportsQ.data || [];
  $: qualSummary = $qualityQ.data || [];
  $: exportSchedules = ($schedQ.data || []).filter(s =>
    ['anomaly_export_daily','remediation_export_weekly'].includes(s.job_id)
  );
</script>

<div class="page">
  <h1 class="page-title">Parquet Exports</h1>

  {#if qualSummary.length > 0}
  <div class="quality-strip">
    {#each qualSummary as q}
    <div class="quality-card">
      <div class="qtype">{q.export_type}</div>
      <span class="badge {qualityColor(q.quality_passed)}">{q.quality_passed ? 'PASS' : q.quality_passed === false ? 'FAIL' : 'N/A'}</span>
      <div class="qmeta">{q.row_count?.toLocaleString() ?? '—'} rows · {age(q.last_export_at)} ago</div>
      <div class="qmeta">Balance {q.class_balance_pct?.toFixed(1) ?? '—'}% · Drift {q.label_drift_score?.toFixed(3) ?? '—'}</div>
    </div>
    {/each}
  </div>
  {/if}

  {#if exportSchedules.length > 0}
  <div class="sched-row">
    {#each exportSchedules as s}
    <div class="sched-chip">
      <span class="mono">{s.job_id}</span>
      <span class="muted">{s.cron_expr}</span>
      <span class="badge {s.enabled ? 'green' : 'gray'}">{s.enabled ? 'ON' : 'OFF'}</span>
    </div>
    {/each}
  </div>
  {/if}

  <section class="section">
    <h2 class="section-title">Export Catalog</h2>
    {#if $exportsQ.isLoading}<p class="muted">Loading…</p>
    {:else}
    <table class="table">
      <thead><tr><th>Type</th><th>Started</th><th>Rows</th><th>Anomaly%</th><th>Schema Hash</th><th>Quality</th><th>Detail</th></tr></thead>
      <tbody>
        {#each exports as ex}
        <tr>
          <td class="mono">{ex.export_type}</td>
          <td class="muted text-sm">{fmt(ex.started_at_ns)}</td>
          <td>{ex.row_count?.toLocaleString() ?? '—'}</td>
          <td>{ex.anomaly_pct?.toFixed(1) ?? '—'}%</td>
          <td class="mono text-sm hash">{(ex.schema_hash || '').slice(0, 10)}</td>
          <td><span class="badge {qualityColor(ex.quality_passed)}">{ex.quality_passed ? 'PASS' : ex.quality_passed === false ? 'FAIL' : '—'}</span></td>
          <td><button class="btn-sm" on:click={() => detailExport = detailExport?.id === ex.id ? null : ex}>Detail</button></td>
        </tr>
        {#if detailExport?.id === ex.id}
        <tr class="detail-row">
          <td colspan="7">
            <div class="detail-panel">
              <div class="detail-grid">
                <div><span class="dlabel">Path</span><span class="mono text-sm">{ex.output_path || '—'}</span></div>
                <div><span class="dlabel">Label drift</span>{ex.label_drift_score?.toFixed(4) ?? '—'}</div>
                <div><span class="dlabel">Worst PSI col</span>{ex.feature_drift_worst_column || '—'} ({ex.feature_drift_worst_psi?.toFixed(3) ?? '—'})</div>
                <div><span class="dlabel">Missing cols</span>{ex.missing_column_list || 'none'}</div>
                <div><span class="dlabel">Model trained on this</span>{ex.model_trained_on_this ? 'Yes' : 'No'}</div>
              </div>
            </div>
          </td>
        </tr>
        {/if}
        {/each}
      </tbody>
    </table>
    {/if}
  </section>
</div>

<style>
  .page-title { font-size: 20px; font-weight: 700; margin: 0 0 20px; }
  .quality-strip { display: flex; gap: 12px; flex-wrap: wrap; margin-bottom: 16px; }
  .quality-card { background: var(--bg-surface, #161b22); border: 1px solid var(--border, #30363d); border-radius: 8px; padding: 14px 18px; min-width: 200px; }
  .qtype { font-size: 11px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-secondary, #8b949e); margin-bottom: 6px; }
  .qmeta { font-size: 12px; color: var(--text-secondary, #8b949e); margin-top: 4px; }
  .sched-row { display: flex; gap: 10px; margin-bottom: 20px; flex-wrap: wrap; }
  .sched-chip { display: flex; align-items: center; gap: 8px; background: var(--bg-surface, #161b22); border: 1px solid var(--border, #30363d); border-radius: 6px; padding: 6px 12px; font-size: 12px; }
  .section { margin-bottom: 24px; }
  .section-title { font-size: 13px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-secondary, #8b949e); margin: 0 0 10px; }
  .table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .table th { text-align: left; padding: 6px 10px; border-bottom: 1px solid var(--border, #30363d); color: var(--text-secondary, #8b949e); font-size: 11px; text-transform: uppercase; }
  .table td { padding: 7px 10px; border-bottom: 1px solid var(--border, #30363d); }
  .mono { font-family: 'JetBrains Mono', monospace; }
  .muted { color: var(--text-secondary, #8b949e); }
  .text-sm { font-size: 12px; }
  .hash { opacity: 0.6; }
  .badge { padding: 2px 7px; border-radius: 4px; font-size: 11px; font-weight: 600; }
  .badge.green { background: rgba(63,185,80,0.15); color: #3fb950; }
  .badge.red   { background: rgba(248,81,73,0.15); color: #f85149; }
  .badge.gray  { background: rgba(139,148,158,0.1); color: #8b949e; }
  .btn-sm { padding: 3px 10px; font-size: 11px; border: 1px solid var(--border, #30363d); background: transparent; color: var(--text-primary, #e6edf3); border-radius: 4px; cursor: pointer; }
  .btn-sm:hover { background: var(--bg-hover, #21262d); }
  .detail-row td { background: var(--bg-hover, #21262d); padding: 0; }
  .detail-panel { padding: 14px 16px; }
  .detail-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 10px; font-size: 12px; }
  .dlabel { display: block; color: var(--text-secondary, #8b949e); font-size: 11px; margin-bottom: 2px; }
</style>
