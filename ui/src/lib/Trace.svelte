<script>
  import { onMount } from 'svelte';

  let { id } = $props();

  let steps = $state([]);
  let loading = $state(true);
  let error = $state(null);

  const KIND_COLOR = {
    trigger:     '#58a6ff',
    detection:   '#f85149',
    remediation: '#3fb950',
  };

  function fmt(ns) {
    if (!ns) return '—';
    return new Date(ns / 1e6).toISOString().replace('T', ' ').replace('Z', '');
  }

  async function load() {
    if (!id) { loading = false; return; }
    try {
      const r = await fetch(`/api/trace/${encodeURIComponent(id)}`);
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      steps = data.steps;
      error = null;
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  onMount(load);
</script>

<div class="view">
  <h2 style="margin-bottom: 16px;">
    Closed-Loop Trace
    {#if id}<span style="color:var(--muted); font-size:12px; font-weight:400; margin-left:8px; font-family:monospace">{id}</span>{/if}
  </h2>

  {#if !id}
    <p class="empty">No detection selected. Click "View trace" from the Events feed.</p>
  {:else if loading}
    <p class="empty">Loading trace...</p>
  {:else if error}
    <p class="empty" style="color:var(--red)">Error: {error}</p>
  {:else if !steps.length}
    <p class="empty">No trace steps found for this detection.</p>
  {:else}
    <div class="card">
      {#each steps as step}
        <div class="trace-step">
          <div style="min-width:120px; color:{KIND_COLOR[step.kind] || 'var(--text)'}; font-weight:600; font-size:12px; text-transform:uppercase;">
            {step.kind}
          </div>
          <div style="flex:1;">
            <div style="font-weight:600">{step.label}</div>
            {#if step.detail}
              <div style="color:var(--muted); font-size:12px; margin-top:2px; font-family:monospace; white-space:pre-wrap; word-break:break-all;">
                {step.detail}
              </div>
            {/if}
            <div style="color:var(--muted); font-size:11px; margin-top:4px;">{fmt(step.occurred_at_ns)}</div>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>
