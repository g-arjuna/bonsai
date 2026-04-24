<script>
  import { onMount } from 'svelte';
  import { relativeTime, absoluteTime, shortTime } from '$lib/timeutil.js';
  import { navigate } from '$lib/router.svelte.js';

  let { address, onclose } = $props();

  let device = $state(null);
  let loading = $state(true);
  let error = $state(null);
  let activeTab = $state('interfaces');

  const TABS = ['interfaces', 'peers', 'paths', 'events', 'detections', 'audit'];

  $effect(() => {
    if (address) {
      loading = true;
      error = null;
      device = null;
      fetch('/api/devices/' + encodeURIComponent(address))
        .then(r => r.ok ? r.json() : r.text().then(t => { throw new Error(t); }))
        .then(d => { device = d; loading = false; })
        .catch(e => { error = e.message; loading = false; });
    }
  });

  function healthClass(h) {
    if (h === 'healthy') return 'healthy';
    if (h === 'critical') return 'critical';
    return 'warn';
  }

  function fmtBytes(n) {
    if (!n) return '—';
    if (n < 1024) return n + ' B';
    if (n < 1048576) return (n / 1024).toFixed(1) + ' KB';
    if (n < 1073741824) return (n / 1048576).toFixed(1) + ' MB';
    return (n / 1073741824).toFixed(2) + ' GB';
  }
</script>

<button
  class="drawer-backdrop"
  onclick={onclose}
  aria-label="Close drawer backdrop"
></button>

<aside class="drawer">
  <div class="drawer-header">
    {#if loading}
      <div class="drawer-title-skeleton"></div>
    {:else if device}
      <div class="drawer-title">
        <span class="badge {healthClass(device.health)}" style="margin-right:8px;">{device.health}</span>
        <strong>{device.hostname || device.address}</strong>
        {#if device.hostname && device.hostname !== device.address}
          <span class="muted" style="font-size:12px; margin-left:6px;">{device.address}</span>
        {/if}
      </div>
      <div class="drawer-meta">
        {#if device.vendor}<span class="meta-chip">{device.vendor}</span>{/if}
        {#if device.role}<span class="meta-chip">{device.role}</span>{/if}
        {#if device.site}<span class="meta-chip">📍 {device.site}</span>{/if}
        {#if device.collector_id}<span class="meta-chip">⇄ {device.collector_id}</span>{/if}
      </div>
    {:else if error}
      <div class="drawer-title muted">Error loading device</div>
    {/if}
    <button class="drawer-close ghost" onclick={onclose} aria-label="Close drawer">✕</button>
  </div>

  {#if error}
    <div class="notice error" style="margin: 12px 16px;">{error}</div>
  {:else if loading}
    <div class="drawer-loading">
      {#each [1, 2, 3] as _}
        <div class="skeleton-line"></div>
      {/each}
    </div>
  {:else if device}
    <div class="drawer-tabs">
      {#each TABS as tab}
        <button class:active={activeTab === tab} onclick={() => (activeTab = tab)}>
          {tab}
        </button>
      {/each}
    </div>

    <div class="drawer-body">
      {#if activeTab === 'interfaces'}
        {#if device.interfaces.length === 0}
          <div class="empty">No interfaces recorded yet.</div>
        {:else}
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>In errors</th>
                <th>Out errors</th>
                <th>In octets</th>
                <th>Out octets</th>
                <th>Updated</th>
              </tr>
            </thead>
            <tbody>
              {#each device.interfaces as iface}
                <tr>
                  <td><code>{iface.name}</code></td>
                  <td class="{iface.in_errors > 0 ? 'text-warn' : ''}">{iface.in_errors}</td>
                  <td class="{iface.out_errors > 0 ? 'text-warn' : ''}">{iface.out_errors}</td>
                  <td>{fmtBytes(iface.in_octets)}</td>
                  <td>{fmtBytes(iface.out_octets)}</td>
                  <td title={absoluteTime(iface.updated_at_ns)}>{relativeTime(iface.updated_at_ns)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}

      {:else if activeTab === 'peers'}
        {#if device.bgp_neighbors.length === 0 && device.lldp_neighbors.length === 0}
          <div class="empty">No peers recorded yet.</div>
        {:else}
          {#if device.bgp_neighbors.length > 0}
            <h4 class="section-head">BGP</h4>
            <table>
              <thead><tr><th>Peer</th><th>AS</th><th>State</th></tr></thead>
              <tbody>
                {#each device.bgp_neighbors as n}
                  <tr>
                    <td><code>{n.peer}</code></td>
                    <td>{n.peer_as || '—'}</td>
                    <td>
                      <span class="badge {n.state === 'established' ? 'healthy' : 'critical'}">{n.state || 'unknown'}</span>
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
          {#if device.lldp_neighbors.length > 0}
            <h4 class="section-head" style="margin-top:16px;">LLDP</h4>
            <table>
              <thead><tr><th>Local port</th><th>Neighbor</th><th>Port ID</th></tr></thead>
              <tbody>
                {#each device.lldp_neighbors as n}
                  <tr>
                    <td><code>{n.local_if}</code></td>
                    <td>{n.system_name || n.chassis_id || '—'}</td>
                    <td><code>{n.port_id || '—'}</code></td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {/if}

      {:else if activeTab === 'paths'}
        {#if device.subscription_statuses.length === 0}
          <div class="empty">No subscription paths.</div>
        {:else}
          <table>
            <thead><tr><th>Path</th><th>Mode</th><th>Status</th><th>Last seen</th></tr></thead>
            <tbody>
              {#each device.subscription_statuses as s}
                <tr>
                  <td><code style="font-size:11px; overflow-wrap:anywhere;">{s.path}</code></td>
                  <td>{s.mode}</td>
                  <td><span class="badge {s.status === 'observed' ? 'healthy' : 'warn'}">{s.status}</span></td>
                  <td title={absoluteTime(s.last_observed_at_ns)}>{relativeTime(s.last_observed_at_ns)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}

      {:else if activeTab === 'events'}
        {#if device.recent_state_changes.length === 0}
          <div class="empty">No state changes recorded yet.</div>
        {:else}
          <div class="event-list">
            {#each device.recent_state_changes as ev}
              <div class="event-row">
                <span class="ts" title={absoluteTime(ev.occurred_at_ns)}>{relativeTime(ev.occurred_at_ns)}</span>
                <div class="body">
                  <span class="evt-type">{ev.event_type.replace(/_/g, ' ')}</span>
                  {#if ev.detail}
                    <span class="muted" style="font-size:12px; display:block; margin-top:2px;">{ev.detail}</span>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        {/if}

      {:else if activeTab === 'detections'}
        {#if device.recent_detections.length === 0}
          <div class="empty">No detections for this device.</div>
        {:else}
          <div class="event-list">
            {#each device.recent_detections as det}
              <button class="det-btn" onclick={() => det.id && navigate('/trace/' + encodeURIComponent(det.id))}>
                <span class="badge {det.severity === 'critical' ? 'critical' : det.severity === 'high' ? 'warn' : 'info'}">{det.severity}</span>
                <span class="det-rule">{det.rule_id || 'detection'}</span>
                <span class="muted det-ts" title={absoluteTime(det.fired_at_ns)}>{relativeTime(det.fired_at_ns)}</span>
                {#if det.remediation_status}
                  <span class="badge {det.remediation_status === 'succeeded' ? 'healthy' : 'warn'}">{det.remediation_status}</span>
                {/if}
              </button>
            {/each}
          </div>
        {/if}

      {:else if activeTab === 'audit'}
        <div class="audit-grid">
          <span class="muted">Created</span>
          <span title={absoluteTime(device.created_at_ns)}>{device.created_at_ns ? relativeTime(device.created_at_ns) : '—'}</span>

          <span class="muted">Created by</span>
          <span>{device.created_by || 'unknown'}</span>

          <span class="muted">Last updated</span>
          <span title={absoluteTime(device.updated_at_ns)}>{device.updated_at_ns ? relativeTime(device.updated_at_ns) : '—'}</span>

          <span class="muted">Updated by</span>
          <span>{device.updated_by || 'unknown'}</span>

          <span class="muted">Last action</span>
          <code>{device.last_operator_action || 'unknown'}</code>
        </div>
      {/if}
    </div>
  {/if}
</aside>

<style>
  .drawer-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0,0,0,0.4);
    z-index: 100;
    border: 0;
    padding: 0;
    width: 100%;
    cursor: pointer;
  }

  .drawer {
    position: fixed;
    top: 0;
    right: 0;
    width: 520px;
    max-width: 90vw;
    height: 100vh;
    background: var(--bg);
    border-left: 1px solid var(--border);
    z-index: 101;
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }

  .drawer-header {
    padding: 16px;
    border-bottom: 1px solid var(--border);
    position: relative;
    padding-right: 44px;
  }

  .drawer-title {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 4px;
    font-size: 15px;
  }

  .drawer-title-skeleton {
    height: 22px;
    width: 200px;
    background: var(--bg2);
    border-radius: 4px;
    animation: pulse 1.5s infinite;
  }

  .audit-grid {
    display: grid;
    grid-template-columns: 110px 1fr;
    gap: 10px 12px;
    font-size: 13px;
  }

  .drawer-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }

  .meta-chip {
    font-size: 11px;
    padding: 2px 7px;
    background: rgba(255,255,255,0.05);
    border: 1px solid var(--border);
    border-radius: 99px;
    color: var(--muted);
  }

  .drawer-close {
    position: absolute;
    top: 14px;
    right: 14px;
    padding: 4px 8px;
    font-size: 14px;
  }

  .drawer-loading {
    padding: 16px;
    display: grid;
    gap: 8px;
  }

  .skeleton-line {
    height: 16px;
    background: var(--bg2);
    border-radius: 4px;
    animation: pulse 1.5s infinite;
  }
  .skeleton-line:nth-child(2) { width: 80%; }
  .skeleton-line:nth-child(3) { width: 60%; }

  @keyframes pulse { 0%, 100% { opacity: 0.6; } 50% { opacity: 0.3; } }

  .drawer-tabs {
    display: flex;
    gap: 2px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--bg2);
    overflow-x: auto;
  }

  .drawer-tabs button {
    background: transparent;
    border: none;
    color: var(--muted);
    padding: 5px 10px;
    border-radius: 4px;
    font-size: 12px;
    text-transform: capitalize;
    cursor: pointer;
  }

  .drawer-tabs button.active {
    background: rgba(88,166,255,0.15);
    color: var(--text);
  }

  .drawer-body {
    flex: 1;
    overflow-y: auto;
    padding: 12px 0;
  }

  .drawer-body table { margin: 0 12px; width: calc(100% - 24px); }

  .section-head {
    font-size: 11px;
    font-weight: 700;
    text-transform: uppercase;
    color: var(--muted);
    letter-spacing: 0.1em;
    padding: 0 12px;
    margin-bottom: 6px;
  }

  .event-list { padding: 0 12px; }

  .event-row {
    display: flex;
    gap: 10px;
    padding: 7px 0;
    border-bottom: 1px solid var(--border);
  }

  .ts { color: var(--muted); font-size: 11px; min-width: 60px; }
  .evt-type { text-transform: capitalize; font-size: 13px; }

  .det-btn {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 0;
    border: none;
    border-bottom: 1px solid var(--border);
    background: transparent;
    color: var(--text);
    cursor: pointer;
    text-align: left;
    font-size: 13px;
    font-family: inherit;
  }
  .det-btn:hover { background: rgba(255,255,255,0.03); }

  .det-rule { flex: 1; }
  .det-ts { font-size: 11px; }
  .text-warn { color: var(--yellow); }
  .empty { padding: 24px 16px; color: var(--muted); text-align: center; }
</style>
