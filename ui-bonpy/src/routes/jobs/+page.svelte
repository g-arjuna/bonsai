<script>
  import { createQuery, createMutation, useQueryClient } from '@tanstack/svelte-query';
  import { api } from '$lib/api.js';
  import { lastProgressEvent } from '$lib/sse.js';

  const qc = useQueryClient();
  const jobsQ = createQuery({ queryKey: ['jobs'], queryFn: api.jobs.list, refetchInterval: 10000 });
  const schedQ = createQuery({ queryKey: ['schedules'], queryFn: api.schedules.list });
  const sidecarQ = createQuery({ queryKey: ['sidecar'], queryFn: api.sidecar.status, refetchInterval: 30000 });

  $: schedulerMode = $sidecarQ.data?.scheduler_mode ?? null;
  $: schedulerDegraded = schedulerMode === 'fallback_manual_only' || schedulerMode === 'unavailable';

  const cancelMut = createMutation({
    mutationFn: id => api.jobs.cancel(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['jobs'] }),
  });
  const retryMut = createMutation({
    mutationFn: id => api.jobs.retry(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['jobs'] }),
  });
  const toggleMut = createMutation({
    mutationFn: ({ id, job_id, cron_expr, enabled }) =>
      api.schedules.upsert({ id, job_id, cron_expr, enabled }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['schedules'] }),
  });

  function statusColor(s) {
    return { running: 'blue', succeeded: 'green', failed: 'red', cancelled: 'gray' }[s] || 'gray';
  }
  function fmt(ns) {
    if (!ns) return '—';
    return new Date(ns / 1e6).toLocaleString();
  }
  function dur(ms) {
    if (!ms) return '—';
    if (ms < 1000) return `${ms}ms`;
    if (ms < 60000) return `${(ms/1000).toFixed(1)}s`;
    return `${(ms/60000).toFixed(1)}m`;
  }

  $: progress = $lastProgressEvent?.payload;
  $: jobs = ($jobsQ.data?.jobs ?? $jobsQ.data ?? []).slice().reverse();
  $: deadLetter = jobs.filter(j => j.status === 'failed' && j.run_count > 3);
</script>

<div class="page">
  <h1 class="page-title">Jobs</h1>

  {#if schedulerDegraded}
  <div class="scheduler-warn">
    <span class="warn-icon">⚠</span>
    <div class="warn-body">
      <strong>Scheduler degraded — {schedulerMode === 'unavailable' ? 'job engine not running' : 'APScheduler 4.x not installed'}</strong>
      <span class="warn-sub">
        Automated jobs will {schedulerMode === 'unavailable' ? 'not run' : 'run on a basic 60-second interval without DB persistence or crash recovery'}.
        Fix: <code>pip install 'apscheduler&gt;=4.0.0a1' aiosqlite 'sqlalchemy[asyncio]' sniffio</code>
        then restart the sidecar.
      </span>
    </div>
  </div>
  {/if}

  {#if $schedQ.data}
  <section class="section">
    <h2 class="section-title">Schedules</h2>
    <table class="table">
      <thead><tr><th>Job ID</th><th>Cron</th><th>Last Run</th><th>Next Run</th><th>Outcome</th><th>Enabled</th></tr></thead>
      <tbody>
        {#each ($schedQ.data?.schedules ?? $schedQ.data ?? []) as s}
        <tr>
          <td class="mono">{s.job_id}</td>
          <td class="mono text-sm">{s.cron_expr}</td>
          <td class="text-sm muted">{fmt(s.last_run_at)}</td>
          <td class="text-sm muted">{fmt(s.next_run_at)}</td>
          <td><span class="badge {statusColor(s.last_outcome)}">{s.last_outcome || '—'}</span></td>
          <td>
            <input type="checkbox" checked={s.enabled}
              on:change={e => $toggleMut.mutate({ id: s.id, job_id: s.job_id, cron_expr: s.cron_expr, enabled: e.target.checked })} />
          </td>
        </tr>
        {/each}
      </tbody>
    </table>
  </section>
  {/if}

  {#if progress}
  <section class="section">
    <h2 class="section-title">Active Job Progress</h2>
    <div class="progress-panel">
      <div class="progress-job">{progress.job_id}</div>
      {#if progress.total_steps}
        <div class="progress-bar-wrap">
          <div class="progress-bar" style="width: {progress.pct}%"></div>
        </div>
        <div class="progress-label">{progress.step}/{progress.total_steps} ({progress.pct}%)</div>
      {/if}
      {#if progress.metric_name}
        <div class="metric-live">{progress.metric_name}: <strong>{progress.metric_value?.toFixed(4)}</strong></div>
      {/if}
    </div>
  </section>
  {/if}

  <section class="section">
    <h2 class="section-title">Run History</h2>
    {#if $jobsQ.isLoading}<span class="muted">Loading…</span>
    {:else}
    <table class="table">
      <thead><tr><th>Job</th><th>Status</th><th>Started</th><th>Duration</th><th>AUC</th><th>Rows</th><th>Actions</th></tr></thead>
      <tbody>
        {#each jobs as j}
        <tr>
          <td class="mono">{j.job_type}</td>
          <td><span class="badge {statusColor(j.status)}">{j.status}</span></td>
          <td class="text-sm muted">{fmt(j.started_at_ns)}</td>
          <td class="text-sm">{dur(j.duration_ms)}</td>
          <td class="text-sm">{j.val_auc?.toFixed(3) ?? '—'}</td>
          <td class="text-sm">{j.row_count?.toLocaleString() ?? '—'}</td>
          <td class="actions">
            {#if j.status === 'running'}
              <button class="btn-sm danger" on:click={() => $cancelMut.mutate(j.id)}>Cancel</button>
            {:else if j.status === 'failed'}
              <button class="btn-sm" on:click={() => $retryMut.mutate(j.id)}>Retry</button>
            {/if}
          </td>
        </tr>
        {/each}
      </tbody>
    </table>
    {/if}
  </section>

  {#if deadLetter.length > 0}
  <section class="section">
    <h2 class="section-title dead">Dead Letter Queue ({deadLetter.length})</h2>
    {#each deadLetter as j}
      <div class="dead-row">
        <span class="mono">{j.job_type}</span>
        <span class="muted">{j.error_message}</span>
        <button class="btn-sm" on:click={() => $retryMut.mutate(j.id)}>Retry</button>
      </div>
    {/each}
  </section>
  {/if}
</div>

<style>
  .page-title { font-size: 20px; font-weight: 700; margin: 0 0 20px; }
  .scheduler-warn { display: flex; align-items: flex-start; gap: 10px; background: rgba(210,153,34,0.12); border: 1px solid rgba(210,153,34,0.5); border-radius: 6px; padding: 12px 16px; margin-bottom: 20px; }
  .warn-icon { font-size: 18px; color: #d29922; line-height: 1.4; flex-shrink: 0; }
  .warn-body { display: flex; flex-direction: column; gap: 4px; font-size: 13px; }
  .warn-body strong { color: #d29922; }
  .warn-sub { color: var(--text-secondary, #8b949e); font-size: 12px; }
  .warn-sub code { background: rgba(139,148,158,0.1); padding: 1px 5px; border-radius: 3px; font-family: 'JetBrains Mono', monospace; font-size: 11px; }
  .section { margin-bottom: 28px; }
  .section-title { font-size: 13px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-secondary, #8b949e); margin: 0 0 10px; }
  .section-title.dead { color: #f85149; }
  .table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .table th { text-align: left; padding: 6px 10px; border-bottom: 1px solid var(--border, #30363d); color: var(--text-secondary, #8b949e); font-size: 11px; text-transform: uppercase; }
  .table td { padding: 7px 10px; border-bottom: 1px solid var(--border, #30363d); }
  .mono { font-family: 'JetBrains Mono', monospace; }
  .muted { color: var(--text-secondary, #8b949e); }
  .text-sm { font-size: 12px; }
  .badge { padding: 2px 7px; border-radius: 4px; font-size: 11px; font-weight: 600; }
  .badge.green  { background: rgba(63,185,80,0.15);  color: #3fb950; }
  .badge.red    { background: rgba(248,81,73,0.15);  color: #f85149; }
  .badge.blue   { background: rgba(79,142,247,0.15); color: #4f8ef7; }
  .badge.gray   { background: rgba(139,148,158,0.1); color: #8b949e; }
  .progress-panel { background: var(--bg-surface, #161b22); border: 1px solid var(--border, #30363d); border-radius: 8px; padding: 14px; }
  .progress-job { font-family: 'JetBrains Mono', monospace; font-weight: 600; margin-bottom: 8px; }
  .progress-bar-wrap { height: 6px; background: var(--bg-hover, #21262d); border-radius: 3px; overflow: hidden; }
  .progress-bar { height: 100%; background: var(--accent-primary, #4f8ef7); transition: width 0.3s; }
  .progress-label { font-size: 12px; color: var(--text-secondary, #8b949e); margin-top: 4px; }
  .metric-live { font-size: 13px; margin-top: 6px; }
  .actions { display: flex; gap: 6px; }
  .btn-sm { padding: 3px 10px; font-size: 11px; border: 1px solid var(--border, #30363d); background: transparent; color: var(--text-primary, #e6edf3); border-radius: 4px; cursor: pointer; }
  .btn-sm:hover { background: var(--bg-hover, #21262d); }
  .btn-sm.danger { border-color: #f85149; color: #f85149; }
  .dead-row { display: flex; gap: 12px; align-items: center; padding: 8px 0; border-bottom: 1px solid var(--border, #30363d); font-size: 13px; }
</style>
