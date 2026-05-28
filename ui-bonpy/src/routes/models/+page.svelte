<script>
  import { createQuery, createMutation, useQueryClient } from '@tanstack/svelte-query';
  import { api } from '$lib/api.js';

  const qc = useQueryClient();
  const modelsQ = createQuery({ queryKey: ['models'], queryFn: api.models.list });
  const activateMut = createMutation({
    mutationFn: id => api.models.activate(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['models'] }),
  });

  let compareIds = [];

  function fmt(ns) { return ns ? new Date(ns / 1e6).toLocaleDateString() : '—'; }

  $: models = $modelsQ.data?.models ?? $modelsQ.data ?? [];
  $: comparison = models.filter(m => compareIds.includes(m.id)).slice(0, 2);
</script>

<div class="page">
  <h1 class="page-title">Model Registry</h1>

  {#if $modelsQ.isLoading}<p class="muted">Loading…</p>
  {:else if $modelsQ.isError}<p class="muted error">Failed to load models: {$modelsQ.error?.message}</p>
  {:else if models.length === 0}<p class="muted">No models registered yet. Run the GNN training pipeline first.</p>
  {:else}
  <table class="table">
    <thead>
      <tr><th>Type</th><th>Version</th><th>AUC</th><th>F1</th><th>Threshold</th><th>Trained</th><th>Status</th><th>Cmp</th><th></th></tr>
    </thead>
    <tbody>
      {#each models as m}
      <tr class="{m.is_active ? 'active-row' : ''}">
        <td class="mono">{m.model_type}</td>
        <td class="mono text-sm">{m.version}</td>
        <td>{m.val_auc?.toFixed(3) ?? '—'}</td>
        <td>{m.val_f1?.toFixed(3) ?? '—'}</td>
        <td class="mono text-sm">{m.threshold ?? '—'}</td>
        <td class="muted text-sm">{fmt(m.trained_at_ns)}</td>
        <td>{#if m.is_active}<span class="badge green">ACTIVE</span>{:else}<span class="badge gray">retired</span>{/if}</td>
        <td><input type="checkbox" value={m.id} bind:group={compareIds} /></td>
        <td>
          {#if !m.is_active}
            <button class="btn-sm" on:click={() => $activateMut.mutate(m.id)}>Activate</button>
          {/if}
        </td>
      </tr>
      {/each}
    </tbody>
  </table>
  {/if}

  {#if comparison.length === 2}
  <section class="section">
    <h2 class="section-title">Side-by-side Comparison</h2>
    <table class="table compare-table">
      <thead><tr><th>Metric</th><th>{comparison[0].version}</th><th>{comparison[1].version}</th></tr></thead>
      <tbody>
        {#each [['val_auc','AUC'],['val_f1','F1'],['val_precision','Precision'],['val_recall','Recall'],['threshold','Threshold']] as [key, label]}
        <tr>
          <td class="muted">{label}</td>
          <td class:better={comparison[0][key] > comparison[1][key]}>{comparison[0][key]?.toFixed(3) ?? '—'}</td>
          <td class:better={comparison[1][key] > comparison[0][key]}>{comparison[1][key]?.toFixed(3) ?? '—'}</td>
        </tr>
        {/each}
      </tbody>
    </table>
  </section>
  {/if}
</div>

<style>
  .page-title { font-size: 20px; font-weight: 700; margin: 0 0 20px; }
  .section { margin-top: 28px; }
  .section-title { font-size: 13px; text-transform: uppercase; letter-spacing: 0.06em; color: var(--text-secondary, #8b949e); margin: 0 0 10px; }
  .table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .table th { text-align: left; padding: 6px 10px; border-bottom: 1px solid var(--border, #30363d); color: var(--text-secondary, #8b949e); font-size: 11px; text-transform: uppercase; }
  .table td { padding: 7px 10px; border-bottom: 1px solid var(--border, #30363d); }
  .active-row td { background: rgba(79,142,247,0.04); }
  .mono { font-family: 'JetBrains Mono', monospace; }
  .muted { color: var(--text-secondary, #8b949e); }
  .error { color: #f85149; }
  .text-sm { font-size: 12px; }
  .badge { padding: 2px 7px; border-radius: 4px; font-size: 11px; font-weight: 600; }
  .badge.green { background: rgba(63,185,80,0.15); color: #3fb950; }
  .badge.gray  { background: rgba(139,148,158,0.1); color: #8b949e; }
  .btn-sm { padding: 3px 10px; font-size: 11px; border: 1px solid var(--border, #30363d); background: transparent; color: var(--text-primary, #e6edf3); border-radius: 4px; cursor: pointer; }
  .btn-sm:hover { background: var(--bg-hover, #21262d); }
  .compare-table { margin-top: 8px; max-width: 420px; }
  .better { color: #3fb950; font-weight: 600; }
</style>
