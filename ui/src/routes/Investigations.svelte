<script>
  import { onMount } from 'svelte';
  import { navigate } from '$lib/router.svelte.js';

  let investigations = $state([]);
  let selected = $state(null);
  let toolCalls = $state([]);
  let loading = $state(true);
  let error = $state('');
  let triggerForm = $state({ detection_id: '', device_address: '', trigger: 'operator' });
  let showTrigger = $state(false);
  let submitting = $state(false);

  onMount(async () => { await loadInvestigations(); });

  async function loadInvestigations() {
    loading = true;
    error = '';
    try {
      const r = await fetch('/api/investigations');
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const data = await r.json();
      investigations = data.investigations ?? [];
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function selectInvestigation(inv) {
    selected = inv;
    toolCalls = [];
    try {
      const r = await fetch(`/api/investigations/${inv.id}`);
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const data = await r.json();
      selected = data.investigation;
      toolCalls = data.tool_calls ?? [];
    } catch (e) {
      error = e.message;
    }
  }

  async function triggerInvestigation() {
    if (!triggerForm.detection_id || !triggerForm.device_address) return;
    submitting = true;
    try {
      const r = await fetch('/api/investigations', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(triggerForm),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      showTrigger = false;
      triggerForm = { detection_id: '', device_address: '', trigger: 'operator' };
      await loadInvestigations();
    } catch (e) {
      error = e.message;
    } finally {
      submitting = false;
    }
  }

  function statusClass(status) {
    return { running: 'status-running', complete: 'status-ok', failed: 'status-err' }[status] ?? '';
  }

  function fmtTime(ns) {
    if (!ns) return '—';
    return new Date(ns / 1e6).toLocaleString();
  }

  function fmtCost(usd) {
    return usd < 0.001 ? '<$0.001' : `$${usd.toFixed(3)}`;
  }

  function hasProposal(inv) {
    return inv?.proposal_json && inv.proposal_json !== '{}' && inv.proposal_json !== '';
  }
</script>

<div class="investigations-root">
  <div class="inv-sidebar">
    <div class="inv-header">
      <span class="inv-title">Investigations</span>
      <button class="btn-sm" onclick={() => showTrigger = !showTrigger}>+ Trigger</button>
    </div>

    {#if showTrigger}
      <div class="trigger-form">
        <input class="form-input" placeholder="detection_id" bind:value={triggerForm.detection_id} />
        <input class="form-input" placeholder="device_address" bind:value={triggerForm.device_address} />
        <div class="trigger-row">
          <button class="btn-primary" onclick={triggerInvestigation} disabled={submitting}>
            {submitting ? 'Triggering…' : 'Start'}
          </button>
          <button class="btn-sm" onclick={() => showTrigger = false}>Cancel</button>
        </div>
      </div>
    {/if}

    {#if error}
      <div class="error-bar">{error}</div>
    {/if}

    {#if loading}
      <div class="loading">Loading…</div>
    {:else if investigations.length === 0}
      <div class="empty-state">No investigations yet.<br>Trigger one manually or wait for an unmatched detection.</div>
    {:else}
      <div class="inv-list">
        {#each investigations as inv (inv.id)}
          <button
            class="inv-item {selected?.id === inv.id ? 'inv-item-active' : ''}"
            onclick={() => selectInvestigation(inv)}
          >
            <div class="inv-item-top">
              <span class="inv-device">{inv.device_address}</span>
              <span class="inv-status {statusClass(inv.status)}">{inv.status}</span>
            </div>
            <div class="inv-item-sub">
              <span class="inv-trigger">{inv.trigger}</span>
              <span class="inv-time">{fmtTime(inv.started_at_ns)}</span>
            </div>
            {#if inv.tokens_used > 0}
              <div class="inv-cost">{inv.tokens_used.toLocaleString()} tok · {fmtCost(inv.cost_usd)}</div>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>

  <div class="inv-detail">
    {#if !selected}
      <div class="detail-empty">Select an investigation to view the reasoning trail.</div>
    {:else}
      <div class="detail-header">
        <div class="detail-title">{selected.device_address}</div>
        <span class="inv-status {statusClass(selected.status)}">{selected.status}</span>
      </div>
      <div class="detail-meta">
        Detection: <code>{selected.detection_id}</code> ·
        Trigger: {selected.trigger} ·
        Started: {fmtTime(selected.started_at_ns)}
        {#if selected.completed_at_ns > 0}
          · Completed: {fmtTime(selected.completed_at_ns)}
        {/if}
        {#if selected.tokens_used > 0}
          · {selected.tokens_used.toLocaleString()} tokens · {fmtCost(selected.cost_usd)}
        {/if}
      </div>

      <!-- Reasoning trail -->
      {#if toolCalls.length > 0}
        <div class="section-label">Reasoning Trail</div>
        <div class="trail">
          {#each toolCalls as tc (tc.id)}
            <div class="trail-step">
              <div class="trail-tool">{tc.tool_name}</div>
              <div class="trail-time">{fmtTime(tc.called_at_ns)}</div>
              <div class="trail-input">
                <span class="trail-label">Input</span>
                <pre class="trail-json">{JSON.stringify(JSON.parse(tc.input_json || '{}'), null, 2)}</pre>
              </div>
              <div class="trail-output">
                <span class="trail-label">Output</span>
                <pre class="trail-json">{JSON.stringify(JSON.parse(tc.output_json || '{}'), null, 2)}</pre>
              </div>
            </div>
          {/each}
        </div>
      {:else if selected.status === 'running'}
        <div class="running-hint">Investigation in progress…</div>
      {/if}

      <!-- Summary -->
      {#if selected.summary}
        <div class="section-label">Summary</div>
        <div class="summary-box">{selected.summary}</div>
      {/if}

      <!-- Proposal -->
      {#if hasProposal(selected)}
        <div class="section-label">Playbook Proposal</div>
        <div class="proposal-box">
          <pre class="trail-json">{JSON.stringify(JSON.parse(selected.proposal_json), null, 2)}</pre>
          <div class="proposal-note">
            ⚠ Proposals require operator approval before execution.
            <a href="#/approvals" onclick={(e) => { e.preventDefault(); navigate('/approvals'); }}>
              Go to Approvals →
            </a>
          </div>
        </div>
      {/if}

      <!-- Budget warning -->
      {#if selected.status === 'failed' && selected.summary?.startsWith('Budget exceeded')}
        <div class="budget-warn">
          Budget limit reached. Increase per_investigation or daily limit in bonsai.toml.
        </div>
      {/if}
    {/if}
  </div>
</div>

<style>
  .investigations-root { display: flex; height: 100%; gap: 0; }

  .inv-sidebar {
    width: 280px; min-width: 220px; border-right: 1px solid #333;
    display: flex; flex-direction: column; overflow: hidden;
  }
  .inv-header {
    display: flex; justify-content: space-between; align-items: center;
    padding: 12px 14px; border-bottom: 1px solid #333;
  }
  .inv-title { font-weight: 600; font-size: 0.95rem; }

  .trigger-form {
    padding: 10px 14px; border-bottom: 1px solid #333;
    display: flex; flex-direction: column; gap: 6px;
  }
  .trigger-row { display: flex; gap: 6px; }
  .form-input {
    background: #1a1a1a; border: 1px solid #444; color: #eee;
    padding: 5px 8px; border-radius: 4px; font-size: 0.82rem; width: 100%;
  }

  .inv-list { overflow-y: auto; flex: 1; }
  .inv-item {
    width: 100%; text-align: left; background: transparent; border: none;
    border-bottom: 1px solid #222; padding: 10px 14px; cursor: pointer; color: #ccc;
  }
  .inv-item:hover { background: #1a1a1a; }
  .inv-item-active { background: #1e2a1e !important; border-left: 2px solid #4caf50; }
  .inv-item-top { display: flex; justify-content: space-between; align-items: center; margin-bottom: 2px; }
  .inv-item-sub { display: flex; justify-content: space-between; font-size: 0.76rem; color: #666; }
  .inv-device { font-size: 0.85rem; font-weight: 500; }
  .inv-cost { font-size: 0.73rem; color: #888; margin-top: 2px; }

  .inv-status { font-size: 0.75rem; font-weight: 600; border-radius: 3px; padding: 1px 6px; }
  .status-running { background: #1a3a5a; color: #7bc8ff; }
  .status-ok  { background: #1a3a1a; color: #7dff7d; }
  .status-err { background: #3a1a1a; color: #ff7d7d; }

  .inv-detail { flex: 1; overflow-y: auto; padding: 20px 24px; }
  .detail-empty { color: #555; margin-top: 40px; text-align: center; }
  .detail-header { display: flex; align-items: center; gap: 10px; margin-bottom: 8px; }
  .detail-title { font-size: 1.1rem; font-weight: 600; }
  .detail-meta { font-size: 0.8rem; color: #888; margin-bottom: 16px; }
  .detail-meta code { font-family: monospace; color: #aaa; }

  .section-label { font-size: 0.78rem; font-weight: 600; color: #888; text-transform: uppercase;
    letter-spacing: 0.05em; margin: 18px 0 8px; }

  .trail { display: flex; flex-direction: column; gap: 10px; }
  .trail-step { border: 1px solid #2a2a2a; border-radius: 6px; overflow: hidden; }
  .trail-tool { background: #1a2a1a; color: #7dff7d; padding: 6px 12px; font-size: 0.82rem; font-weight: 600; }
  .trail-time { font-size: 0.72rem; color: #666; padding: 2px 12px; }
  .trail-input, .trail-output { padding: 8px 12px; }
  .trail-label { font-size: 0.72rem; color: #666; text-transform: uppercase; }
  .trail-json { font-size: 0.78rem; color: #ccc; margin: 4px 0 0; white-space: pre-wrap; word-break: break-all;
    max-height: 140px; overflow-y: auto; }

  .summary-box { background: #1a1e1a; border: 1px solid #2d3d2d; border-radius: 6px;
    padding: 14px; color: #ccc; font-size: 0.88rem; line-height: 1.6; white-space: pre-wrap; }

  .proposal-box { background: #1e1a10; border: 1px solid #4a3a10; border-radius: 6px; padding: 14px; }
  .proposal-note { margin-top: 10px; font-size: 0.8rem; color: #c8a000; }
  .proposal-note a { color: #ffcc44; }

  .budget-warn { background: #2a1010; border: 1px solid #5a2020; border-radius: 6px;
    padding: 10px 14px; color: #ff9999; font-size: 0.82rem; margin-top: 12px; }

  .running-hint { color: #666; font-size: 0.85rem; margin-top: 8px; font-style: italic; }
  .loading, .empty-state { color: #555; padding: 20px 14px; font-size: 0.85rem; }

  .btn-sm { background: #2a2a2a; border: 1px solid #444; color: #ccc; padding: 4px 10px;
    border-radius: 4px; cursor: pointer; font-size: 0.8rem; }
  .btn-sm:hover { background: #333; }
  .btn-primary { background: #1a4a1a; border: 1px solid #2d6a2d; color: #7dff7d;
    padding: 5px 12px; border-radius: 4px; cursor: pointer; font-size: 0.82rem; }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }

  .error-bar { background: #2a1010; color: #ff7d7d; font-size: 0.8rem; padding: 8px 14px; }
  .inv-trigger { text-transform: capitalize; }
</style>
