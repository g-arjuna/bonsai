<script>
  import { onMount } from 'svelte';

  let data = $state({ proposals: [], trust: [], graduation_hints: [], active_rollbacks: [] });
  let loading = $state(true);
  let error = $state('');
  let busy = $state('');
  let filter = $state('pending');

  onMount(load);

  async function load() {
    loading = true;
    error = '';
    try {
      const r = await fetch(`/api/approvals?status=${encodeURIComponent(filter)}&limit=100`);
      if (!r.ok) throw new Error(await r.text());
      data = await r.json();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function decide(id, action) {
    const note = window.prompt(action === 'approve' ? 'Approval note' : action === 'reject' ? 'Rejection note' : 'Rollback note', '');
    if (note === null) return;
    busy = id;
    try {
      const r = await fetch(`/api/approvals/${encodeURIComponent(id)}/${action}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ operator_note: note }),
      });
      const body = await r.json();
      if (!body.success) throw new Error(body.error || `${action} failed`);
      await load();
    } catch (e) {
      error = e.message;
    } finally {
      busy = '';
    }
  }

  async function graduate(hint) {
    busy = hint.trust_key;
    try {
      const r = await fetch('/api/trust/graduate', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          trust_key: hint.trust_key,
          to_state: hint.to_state,
          operator_note: hint.reason,
        }),
      });
      const body = await r.json();
      if (!body.success) throw new Error(body.error || 'graduation failed');
      await load();
    } catch (e) {
      error = e.message;
    } finally {
      busy = '';
    }
  }

  function fmt(ns) {
    if (!ns) return '-';
    return new Date(Math.floor(ns / 1_000_000)).toLocaleString();
  }

  function stateLabel(record) {
    return (record?.state ?? 'approve_each').replaceAll('_', ' ');
  }

  function parseSteps(json) {
    try {
      const parsed = JSON.parse(json || '[]');
      return Array.isArray(parsed) ? parsed : [parsed];
    } catch (_) {
      return [];
    }
  }

  function rollbackActive(id) {
    return (data.active_rollbacks ?? []).some((r) => r.proposal_id === id && !r.rolled_back);
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Remediation</p>
      <h2>Approvals</h2>
    </div>
    <div class="toolbar">
      <select bind:value={filter} onchange={load}>
        <option value="pending">Pending</option>
        <option value="approved">Approved</option>
        <option value="rejected">Rejected</option>
        <option value="rolled_back">Rolled back</option>
        <option value="all">All</option>
      </select>
      <button class="ghost" onclick={load}>Refresh</button>
    </div>
  </div>

  {#if loading}
    <div class="card skeleton"></div>
  {:else if error}
    <div class="notice error">{error}</div>
  {:else}
    {#if data.graduation_hints?.length}
      <div class="card section">
        <h3>Graduation suggestions</h3>
        {#each data.graduation_hints as hint}
          <div class="hint">
            <div>
              <strong>{hint.from_state.replaceAll('_', ' ')} -> {hint.to_state.replaceAll('_', ' ')}</strong>
              <p>{hint.reason}</p>
              <code>{hint.trust_key}</code>
            </div>
            <button disabled={busy === hint.trust_key} onclick={() => graduate(hint)}>Accept</button>
          </div>
        {/each}
      </div>
    {/if}

    <div class="approval-grid">
      {#if !data.proposals?.length}
        <div class="empty">No proposals match this view.</div>
      {:else}
        {#each data.proposals as p}
          <article class="card proposal">
            <div class="proposal-head">
              <div>
                <span class="badge {p.status === 'pending' ? 'warn' : p.status === 'rejected' ? 'critical' : 'healthy'}">{p.status.replaceAll('_', ' ')}</span>
                <h3>{p.playbook_id}</h3>
                <p>{p.device_address || 'unknown device'} · {p.rule_id || 'unknown rule'} · {p.severity || 'unknown severity'}</p>
              </div>
              <span class="time">{fmt(p.proposed_at_ns)}</span>
            </div>

            <div class="detail-grid">
              <div><span>Detection</span><code>{p.detection_id}</code></div>
              <div><span>Trust key</span><code>{p.trust_key}</code></div>
            </div>

            {#if parseSteps(p.steps_json).length}
              <div class="steps">
                {#each parseSteps(p.steps_json) as step, i}
                  <div><span>{i + 1}</span><code>{JSON.stringify(step)}</code></div>
                {/each}
              </div>
            {/if}

            {#if p.operator_note}
              <p class="note">{p.operator_note}</p>
            {/if}

            <div class="actions">
              {#if p.status === 'pending'}
                <button disabled={busy === p.id} onclick={() => decide(p.id, 'approve')}>Approve</button>
                <button class="danger" disabled={busy === p.id} onclick={() => decide(p.id, 'reject')}>Reject</button>
              {/if}
              {#if rollbackActive(p.id)}
                <button class="ghost danger" disabled={busy === p.id} onclick={() => decide(p.id, 'rollback')}>Rollback</button>
              {/if}
            </div>
          </article>
        {/each}
      {/if}
    </div>

    <div class="card section">
      <h3>Trust tuples</h3>
      {#if !data.trust?.length}
        <div class="empty">No trust history yet.</div>
      {:else}
        <table>
          <thead><tr><th>Tuple</th><th>State</th><th>Approvals</th><th>Rejections</th><th>Success streak</th><th>Updated</th></tr></thead>
          <tbody>
            {#each data.trust as t}
              <tr>
                <td><code>{t.trust_key}</code></td>
                <td><span class="badge info">{stateLabel(t.record)}</span></td>
                <td>{t.record.operator_approvals ?? 0}</td>
                <td>{t.record.operator_rejections ?? 0}</td>
                <td>{t.record.consecutive_successes ?? 0}</td>
                <td>{fmt(t.record.updated_at_ns)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  {/if}
</div>

<style>
  @keyframes pulse { 0%, 100% { opacity: 0.4; } 50% { opacity: 0.2; } }
  .skeleton { height: 180px; animation: pulse 1.5s infinite; }
  .toolbar { display: flex; gap: 8px; align-items: center; }
  .toolbar select { min-width: 130px; }
  .section { padding: 16px; margin-bottom: 16px; }
  .section h3 { margin: 0 0 12px; font-size: 14px; }
  .approval-grid { display: grid; gap: 12px; }
  .proposal { padding: 16px; }
  .proposal-head { display: flex; justify-content: space-between; gap: 12px; align-items: flex-start; }
  .proposal h3 { margin: 8px 0 4px; font-size: 16px; }
  .proposal p { margin: 0; color: var(--muted); font-size: 13px; }
  .time { color: var(--muted); font-size: 12px; white-space: nowrap; }
  .detail-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(220px, 1fr)); gap: 10px; margin: 14px 0; }
  .detail-grid span { display: block; color: var(--muted); font-size: 11px; margin-bottom: 4px; }
  code { word-break: break-word; }
  .steps { display: grid; gap: 6px; margin-top: 10px; }
  .steps div { display: flex; gap: 8px; align-items: flex-start; padding: 8px; background: var(--bg2); border-radius: 6px; }
  .steps span { color: var(--muted); font-size: 12px; width: 18px; flex: 0 0 auto; }
  .note { margin-top: 12px !important; padding: 8px; border-left: 2px solid var(--border); }
  .actions { display: flex; gap: 8px; justify-content: flex-end; margin-top: 12px; }
  .hint { display: flex; justify-content: space-between; gap: 12px; padding: 10px 0; border-top: 1px solid var(--border); }
  .hint:first-of-type { border-top: 0; }
  .hint p { margin: 3px 0 5px; color: var(--muted); font-size: 13px; }
  .danger { color: var(--red, #f85149); border-color: color-mix(in srgb, var(--red, #f85149) 40%, var(--border)); }
  @media (max-width: 720px) {
    .proposal-head, .hint { flex-direction: column; }
    .actions { justify-content: flex-start; flex-wrap: wrap; }
  }
</style>
