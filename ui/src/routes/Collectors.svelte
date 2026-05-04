<script>
  import { onMount } from 'svelte';
  import { toast } from '$lib/toast.svelte.js';
  import { relativeTime, absoluteTime, duration } from '$lib/timeutil.js';

  let status = $state(null);
  let loading = $state(true);
  let error = $state(null);
  let overrideAddress = $state('');
  let overrideCollector = $state('');
  let saving = $state(false);

  onMount(() => {
    loadStatus();

    // Subscribe to SSE for live collector status updates.
    let es;
    try {
      es = new EventSource('/api/events');
      es.onmessage = (e) => {
        try {
          const ev = JSON.parse(e.data);
          if (ev.event_type === 'collector_status_change') loadStatus();
        } catch {}
      };
    } catch {}

    // 60s polling fallback (covers browsers without SSE and missed events).
    const poll = setInterval(loadStatus, 60_000);

    return () => {
      clearInterval(poll);
      if (es) es.close();
    };
  });

  async function loadStatus() {
    loading = true;
    error = null;
    try {
      const r = await fetch('/api/collectors');
      if (!r.ok) throw new Error(await r.text());
      status = await r.json();
    } catch (e) {
      error = e.message;
      status = null;
    } finally {
      loading = false;
    }
  }

  async function applyOverride() {
    if (!overrideAddress.trim()) return;
    saving = true;
    try {
      const r = await fetch('/api/assignment/override', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          device_address: overrideAddress.trim(),
          collector_id: overrideCollector.trim() || null,
        }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(
        overrideCollector.trim()
          ? `Assigned ${overrideAddress} -> ${overrideCollector}`
          : `Cleared assignment for ${overrideAddress}`,
        'success'
      );
      overrideAddress = '';
      overrideCollector = '';
      await loadStatus();
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      saving = false;
    }
  }

  function observedRatio(col) {
    const total = (col.observed_subscriptions ?? 0) + (col.pending_subscriptions ?? 0) + (col.silent_subscriptions ?? 0);
    if (!total) return '—';
    return `${Math.round(((col.observed_subscriptions ?? 0) / total) * 100)}%`;
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Distributed architecture</p>
      <h2>Collectors</h2>
    </div>
  </div>

  {#if loading}
    <div class="collector-grid">
      {#each [1, 2] as _}
        <div class="card skeleton"></div>
      {/each}
    </div>
  {:else if error}
    <div class="notice error">{error}</div>
  {:else if !status}
    <div class="empty">No collector data available.</div>
  {:else}
    <div class="summary-grid">
      <div class="card metric">
        <span>Collectors</span>
        <strong>{status.collectors?.length ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Connected</span>
        <strong>{(status.collectors ?? []).filter((col) => col.connected).length}</strong>
      </div>
      <div class="card metric">
        <span>Unassigned devices</span>
        <strong>{status.unassigned_count ?? 0}</strong>
      </div>
    </div>

    {#if (status.unassigned_count ?? 0) > 0}
      <div class="notice error" style="margin-top: 16px;">
        {status.unassigned_count} device{status.unassigned_count === 1 ? '' : 's'} unassigned. Use an assignment rule or the manual override below.
      </div>
    {/if}

    <div class="collector-grid" style="margin-top: 16px;">
      {#each status.collectors ?? [] as col (col.id)}
        <div class="card collector-card">
          <div class="collector-header">
            <div>
              <div class="collector-id">{col.id}</div>
              <div class="muted small">
                {#if col.last_heartbeat_ns}
                  last heartbeat {relativeTime(col.last_heartbeat_ns)}
                {:else}
                  no heartbeat yet
                {/if}
              </div>
            </div>
            <span class="badge {col.connected ? 'healthy' : 'critical'}">
              {col.connected ? 'connected' : 'offline'}
            </span>
          </div>

          <div class="collector-stats">
            <div class="stat">
              <span class="stat-label">Assigned</span>
              <span class="stat-val">{col.assigned_device_count}</span>
            </div>
            <div class="stat">
              <span class="stat-label">Queue depth</span>
              <span class="stat-val">{col.queue_depth_updates ?? 0}</span>
            </div>
            <div class="stat">
              <span class="stat-label">Observed ratio</span>
              <span class="stat-val">{observedRatio(col)}</span>
            </div>
          </div>

          <div class="collector-detail-grid">
            <div><span class="muted">Subscriptions</span> {col.subscription_count ?? 0}</div>
            <div><span class="muted">Observed</span> {col.observed_subscriptions ?? 0}</div>
            <div><span class="muted">Pending</span> {col.pending_subscriptions ?? 0}</div>
            <div><span class="muted">Silent</span> {col.silent_subscriptions ?? 0}</div>
            <div><span class="muted">Uptime</span> {col.uptime_secs ? duration(0, col.uptime_secs * 1_000_000_000) : '—'}</div>
            <div><span class="muted">Heartbeat</span> <span title={absoluteTime(col.last_heartbeat_ns)}>{col.last_heartbeat_ns ? relativeTime(col.last_heartbeat_ns) : '—'}</span></div>
          </div>

          {#if col.assigned_targets?.length}
            <div class="target-list">
              {#each col.assigned_targets as target}
                <code class="target-chip">{target}</code>
              {/each}
            </div>
          {/if}
        </div>
      {/each}
      {#if (status.collectors?.length ?? 0) === 0}
        <div class="empty">No collectors registered yet.</div>
      {/if}
    </div>

    {#if (status.unassigned_devices?.length ?? 0) > 0}
      <div class="card" style="margin-top: 16px;">
        <h3 style="margin-bottom: 10px; font-size:15px;">Unassigned devices</h3>
        <div class="target-list no-border">
          {#each status.unassigned_devices as addr}
            <code class="target-chip danger-outline">{addr}</code>
          {/each}
        </div>
      </div>
    {/if}

    <div class="card" style="margin-top: 16px;">
      <h3 style="margin-bottom: 12px; font-size:15px;">Manual assignment override</h3>
      <div class="override-form">
        <input bind:value={overrideAddress} placeholder="Device address" autocomplete="off" />
        <input bind:value={overrideCollector} placeholder="Collector ID (leave blank to clear)" autocomplete="off" />
        <button onclick={applyOverride} disabled={saving || !overrideAddress.trim()}>
          {saving ? 'Applying…' : 'Apply'}
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  @keyframes pulse { 0%, 100% { opacity: 0.4; } 50% { opacity: 0.2; } }

  .skeleton { height: 160px; opacity: 0.4; animation: pulse 1.5s infinite; }
  .summary-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 12px; }
  .collector-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 12px; }
  .collector-card { padding: 16px; }
  .collector-header { display: flex; justify-content: space-between; align-items: start; gap: 12px; margin-bottom: 12px; }
  .collector-id { font-weight: 700; font-size: 16px; }
  .small { font-size: 12px; }
  .collector-stats { display: grid; grid-template-columns: repeat(3, 1fr); gap: 12px; }
  .collector-detail-grid { display: grid; grid-template-columns: repeat(2, 1fr); gap: 8px 14px; margin-top: 12px; font-size: 13px; }
  .stat { display: flex; flex-direction: column; gap: 2px; }
  .stat-label { color: var(--muted); font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.08em; }
  .stat-val { font-size: 22px; font-weight: 700; }
  .target-list { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 12px; border-top: 1px solid var(--border); padding-top: 10px; }
  .target-list.no-border { border-top: 0; padding-top: 0; margin-top: 0; }
  .target-chip { padding: 3px 7px; background: #0b1118; border: 1px solid var(--border); border-radius: 4px; font-size: 12px; }
  .target-chip.danger-outline { border-color: rgba(248,81,73,0.4); }
  .override-form { display: grid; grid-template-columns: 1fr 1fr auto; gap: 8px; align-items: end; }

  @media (max-width: 800px) {
    .collector-stats, .collector-detail-grid, .override-form { grid-template-columns: 1fr; }
  }
</style>
