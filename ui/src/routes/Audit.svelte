<script>
  import { onMount } from 'svelte';

  let entries = $state([]);
  let loading = $state(true);
  let error = $state('');
  let limit = $state(200);

  onMount(load);

  async function load() {
    loading = true;
    error = '';
    try {
      const r = await fetch(`/api/audit?limit=${limit}`);
      if (!r.ok) throw new Error(await r.text());
      const body = await r.json();
      entries = body.entries || [];
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function fmt(ns) {
    if (!ns) return '-';
    return new Date(Math.floor(ns / 1_000_000)).toLocaleString();
  }

  function eventType(entry) {
    if (entry.event) return entry.event.replace(/_/g, ' ');
    if (entry.alias) return 'credential resolve';
    if (entry.enricher_name) return 'enrichment run';
    if (entry.adapter) return 'adapter push';
    return 'audit event';
  }

  function eventSummary(entry) {
    if (entry.event === 'trust_op') {
      return `${entry.operation} · ${entry.trust_key}${entry.operator_note ? ' — ' + entry.operator_note : ''}`;
    }
    if (entry.alias) {
      return `${entry.alias} (${entry.purpose}) → ${entry.outcome}${entry.error ? ' ⚠ ' + entry.error : ''}`;
    }
    if (entry.enricher_name) {
      return `${entry.enricher_name} → ${entry.outcome} · ${entry.nodes_touched ?? 0} nodes`;
    }
    if (entry.adapter) {
      return `${entry.adapter} → ${entry.outcome} · ${entry.events_pushed ?? 0} events`;
    }
    return JSON.stringify(entry);
  }

  function outcomeClass(entry) {
    const o = (entry.outcome || entry.operation || '').toLowerCase();
    if (o === 'ok' || o === 'approve' || o === 'graduate') return 'ok';
    if (o === 'reject' || o === 'error' || o === 'failed') return 'err';
    if (o === 'rollback') return 'warn';
    return 'muted';
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Operations</p>
      <h2>Audit Trail</h2>
    </div>
    <div class="toolbar">
      <select bind:value={limit} onchange={load}>
        <option value={50}>Last 50</option>
        <option value={200}>Last 200</option>
        <option value={500}>Last 500</option>
        <option value={1000}>Last 1000</option>
      </select>
      <button onclick={load}>Refresh</button>
    </div>
  </div>

  {#if error}
    <div class="banner error">{error}</div>
  {/if}

  {#if loading}
    <div class="loading">Loading audit log…</div>
  {:else if !entries.length}
    <div class="empty-state">No audit entries found. Operator actions (approvals, credential resolves, enrichment runs) will appear here.</div>
  {:else}
    <div class="audit-table-wrap">
      <table class="audit-table">
        <thead>
          <tr>
            <th>Time</th>
            <th>Type</th>
            <th>Summary</th>
          </tr>
        </thead>
        <tbody>
          {#each entries as entry}
            <tr>
              <td class="ts">{fmt(entry.timestamp_ns)}</td>
              <td><span class="type-badge type-{outcomeClass(entry)}">{eventType(entry)}</span></td>
              <td class="summary">{eventSummary(entry)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<style>
  .toolbar { display: flex; gap: 8px; align-items: center; }
  .toolbar select { min-width: 110px; }
  .loading, .empty-state { padding: 40px 0; color: var(--muted); font-size: 14px; text-align: center; }
  .audit-table-wrap { overflow-x: auto; }
  .audit-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .audit-table th { text-align: left; padding: 8px 12px; border-bottom: 1px solid var(--border); color: var(--muted); font-weight: 500; font-size: 11px; text-transform: uppercase; letter-spacing: 0.05em; }
  .audit-table td { padding: 8px 12px; border-bottom: 1px solid var(--border); vertical-align: top; }
  .ts { white-space: nowrap; color: var(--muted); font-size: 12px; }
  .summary { font-family: monospace; font-size: 12px; word-break: break-all; }
  .type-badge { font-size: 11px; padding: 2px 7px; border-radius: 4px; font-weight: 600; text-transform: uppercase; letter-spacing: 0.04em; }
  .type-ok   { background: #14532d; color: #4ade80; }
  .type-err  { background: #450a0a; color: #f87171; }
  .type-warn { background: #3a2a1a; color: #f59e0b; }
  .type-muted { background: #1f2937; color: #6b7280; }
  .banner.error { background: #450a0a22; border: 1px solid #f87171; border-radius: 6px; padding: 8px 14px; color: #f87171; margin-bottom: 16px; font-size: 13px; }
</style>
