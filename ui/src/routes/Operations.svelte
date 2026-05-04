<script>
  import { onMount } from 'svelte';

  let ops = $state(null);
  let collectors = $state(null);
  let subscriptions = $state(null);
  let loading = $state(true);
  let error = $state(null);

  // Ring buffer — last 12 samples at 5s interval = 1 minute of history.
  const SPARKLINE_MAX = 12;
  let rssSamples = $state([]);
  let archiveSamples = $state([]);
  let graphSamples = $state([]);

  onMount(() => {
    fetchAll();
    const poll = setInterval(fetchAll, 5_000);
    return () => clearInterval(poll);
  });

  async function fetchAll() {
    try {
      const [opsRes, collRes, topoRes] = await Promise.all([
        fetch('/api/operations'),
        fetch('/api/assignment/status'),
        fetch('/api/topology'),
      ]);
      if (!opsRes.ok) throw new Error(await opsRes.text());
      ops = await opsRes.json();
      if (collRes.ok) collectors = await collRes.json();
      if (topoRes.ok) {
        const topo = await topoRes.json();
        subscriptions = topo.devices ?? [];
      }
      // Update sparkline ring buffers.
      rssSamples = [...rssSamples, ops.rss_bytes ?? 0].slice(-SPARKLINE_MAX);
      archiveSamples = [...archiveSamples, ops.archive_disk_bytes ?? 0].slice(-SPARKLINE_MAX);
      graphSamples = [...graphSamples, ops.graph_disk_bytes ?? 0].slice(-SPARKLINE_MAX);
      error = null;
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function sparklinePath(samples, w, h) {
    if (samples.length < 2) return '';
    const max = Math.max(...samples, 1);
    const pts = samples.map((v, i) => {
      const x = (i / (samples.length - 1)) * w;
      const y = h - (v / max) * h;
      return `${x},${y}`;
    });
    return 'M' + pts.join(' L');
  }

  function subBadgeClass(status) {
    if (status === 'observed') return 'healthy';
    if (status === 'pending') return 'warn';
    return 'critical';
  }

  function collectorBadge(c) {
    return c.connected ? 'healthy' : 'critical';
  }

  function formatUptime(secs) {
    if (!secs) return '—';
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    return h ? `${h}h ${m}m` : `${m}m`;
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">System</p>
      <h2>Operations</h2>
    </div>
    <a href="/metrics" target="_blank" class="ghost-link">Open Prometheus metrics ↗</a>
  </div>

  {#if loading}
    <div class="ops-grid">
      {#each [1, 2, 3, 4, 5, 6] as _}
        <div class="card skeleton"></div>
      {/each}
    </div>
  {:else if error}
    <div class="notice error">{error}</div>
  {:else if !ops}
    <div class="notice error">Core did not return an operations summary.</div>
  {:else}

    <!-- ── Core counters ────────────────────────────────────────────────── -->
    <div class="ops-grid">
      <div class="card metric">
        <span>Detection events</span>
        <strong>{ops.detection_events ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>State changes</span>
        <strong>{ops.state_change_events ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Devices enabled</span>
        <strong>{ops.enabled_device_count ?? 0} / {ops.device_count ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Collectors</span>
        <strong class="{(ops.collectors_connected ?? 0) > 0 ? '' : 'warn-text'}">
          {ops.collectors_connected ?? 0} / {ops.collectors_total ?? 0}
        </strong>
      </div>
      <div class="card metric">
        <span>Observed subscriptions</span>
        <strong>{ops.observed_subscriptions ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Pending subscriptions</span>
        <strong class="{(ops.pending_subscriptions ?? 0) > 0 ? 'warn-text' : ''}">
          {ops.pending_subscriptions ?? 0}
        </strong>
      </div>
      <div class="card metric">
        <span>Silent subscriptions</span>
        <strong class="{(ops.silent_subscriptions ?? 0) > 0 ? 'warn-text' : ''}">
          {ops.silent_subscriptions ?? 0}
        </strong>
      </div>
      <div class="card metric">
        <span>Trusted remediations</span>
        <strong>{ops.remediation_rows_post_cutoff ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Event bus depth</span>
        <strong class="{(ops.event_bus_depth ?? 0) > 800 ? 'warn-text' : ''}">
          {ops.event_bus_depth ?? 0}
        </strong>
      </div>
      <div class="card metric">
        <span>Event bus receivers</span>
        <strong>{ops.event_bus_receivers ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Archive lag</span>
        <strong class="{(ops.archive_lag_millis ?? 0) > 5000 ? 'warn-text' : ''}">
          {ops.archive_lag_millis ?? 0} ms
        </strong>
      </div>
      <div class="card metric">
        <span>Archive buffer</span>
        <strong>{ops.archive_buffer_rows ?? 0} rows</strong>
      </div>
    </div>

    <!-- ── Memory and disk (live sparklines, 5s poll) ───────────────── -->
    <div class="ops-grid" style="margin-top:12px;">
      <div class="card metric sparkline-card">
        <span>RSS memory</span>
        <strong class="{(ops.rss_bytes ?? 0) > 900 * 1024 * 1024 ? 'warn-text' : ''}">
          {Math.round((ops.rss_bytes ?? 0) / 1024 / 1024)} MB
        </strong>
        {#if rssSamples.length > 1}
          <svg class="sparkline" viewBox="0 0 100 24" preserveAspectRatio="none">
            <path d={sparklinePath(rssSamples, 100, 24)} />
          </svg>
        {/if}
      </div>
      <div class="card metric sparkline-card">
        <span>Archive on disk</span>
        <strong class="{(ops.archive_disk_pct ?? 0) >= 80 ? 'warn-text' : ''}">
          {Math.round((ops.archive_disk_bytes ?? 0) / 1024 / 1024)} MB
          {#if (ops.archive_disk_pct ?? 0) > 0}
            <small>({ops.archive_disk_pct}%)</small>
          {/if}
        </strong>
        {#if archiveSamples.length > 1}
          <svg class="sparkline" viewBox="0 0 100 24" preserveAspectRatio="none">
            <path d={sparklinePath(archiveSamples, 100, 24)} />
          </svg>
        {/if}
      </div>
      <div class="card metric sparkline-card">
        <span>Graph DB on disk</span>
        <strong class="{(ops.graph_disk_pct ?? 0) >= 80 ? 'warn-text' : ''}">
          {Math.round((ops.graph_disk_bytes ?? 0) / 1024 / 1024)} MB
          {#if (ops.graph_disk_pct ?? 0) > 0}
            <small>({ops.graph_disk_pct}%)</small>
          {/if}
        </strong>
        {#if graphSamples.length > 1}
          <svg class="sparkline" viewBox="0 0 100 24" preserveAspectRatio="none">
            <path d={sparklinePath(graphSamples, 100, 24)} />
          </svg>
        {/if}
      </div>
    </div>

    <!-- ── Collector health ────────────────────────────────────────────── -->
    {#if collectors?.collectors?.length}
      <div class="card section">
        <h3>Collector health</h3>
        <table>
          <thead>
            <tr>
              <th>Collector</th>
              <th>Status</th>
              <th>Devices</th>
              <th>Subscriptions</th>
              <th>Queue depth</th>
              <th>Uptime</th>
            </tr>
          </thead>
          <tbody>
            {#each collectors.collectors as c}
              <tr>
                <td><code>{c.id}</code></td>
                <td><span class="badge {collectorBadge(c)}">{c.connected ? 'connected' : 'disconnected'}</span></td>
                <td>{c.assigned_device_count}</td>
                <td>
                  <span class="badge healthy" title="observed">{c.observed_subscriptions ?? 0} obs</span>
                  {#if (c.pending_subscriptions ?? 0) > 0}
                    <span class="badge warn" title="pending">{c.pending_subscriptions} pend</span>
                  {/if}
                  {#if (c.silent_subscriptions ?? 0) > 0}
                    <span class="badge critical" title="silent">{c.silent_subscriptions} silent</span>
                  {/if}
                </td>
                <td>{c.queue_depth_updates ?? 0}</td>
                <td>{formatUptime(c.uptime_secs)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
        {#if collectors.unassigned_count > 0}
          <div class="notice warn" style="margin-top:8px;">
            {collectors.unassigned_count} device(s) unassigned to any collector.
          </div>
        {/if}
      </div>
    {/if}

    <!-- ── Subscriber health per device ───────────────────────────────── -->
    {#if subscriptions?.length}
      <div class="card section">
        <h3>Subscription health per device</h3>
        <table>
          <thead>
            <tr><th>Device</th><th>Health</th><th>BGP peers</th><th>Subscriptions</th></tr>
          </thead>
          <tbody>
            {#each subscriptions as dev}
              <tr>
                <td>
                  <strong>{dev.hostname || dev.address}</strong><br>
                  <span class="muted" style="font-size:11px">{dev.address}</span>
                </td>
                <td><span class="badge {dev.health}">{dev.health}</span></td>
                <td>
                  {#if dev.bgp?.length}
                    {dev.bgp.filter(b => b.state === 'established').length}/{dev.bgp.length}
                    established
                  {:else}
                    <span class="muted">—</span>
                  {/if}
                </td>
                <td>
                  <span class="muted" style="font-size:12px">
                    {dev.role || 'unknown role'} · {dev.site || 'no site'}
                  </span>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}

    <!-- ── Diagnostics panels ────────────────────────────────────────── -->
    <div class="ops-sections">
      <div class="card">
        <h3>Rule engine activity</h3>
        {#if Object.keys(ops.rule_distribution ?? {}).length === 0}
          <div class="empty">No rule activity recorded yet.</div>
        {:else}
          <table>
            <thead><tr><th>Rule ID</th><th>Detections</th></tr></thead>
            <tbody>
              {#each Object.entries(ops.rule_distribution ?? {}).sort((a, b) => b[1] - a[1]) as [rule, count]}
                <tr>
                  <td><code>{rule}</code></td>
                  <td>{count}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>

      <div class="card">
        <h3>Remediation outcomes</h3>
        {#if Object.keys(ops.status_distribution_post_cutoff ?? {}).length === 0}
          <div class="empty">No remediation outcomes recorded yet.</div>
        {:else}
          <table>
            <thead><tr><th>Status</th><th>Count</th></tr></thead>
            <tbody>
              {#each Object.entries(ops.status_distribution_post_cutoff ?? {}) as [status, count]}
                <tr>
                  <td><span class="badge {status === 'succeeded' ? 'healthy' : status === 'failed' ? 'critical' : 'info'}">{status}</span></td>
                  <td>{count}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>

      <div class="card">
        <h3>Operator checklist</h3>
        <div class="check-grid">
          <div><span class="muted">Unassigned devices</span>
            <strong class="{(ops.unassigned_devices ?? 0) > 0 ? 'warn-text' : ''}">
              {ops.unassigned_devices ?? 0}
            </strong>
          </div>
          <div><span class="muted">Trust cutoff</span> <code>{ops.cutoff_iso}</code></div>
          <div><span class="muted">Last archive flush</span> {ops.archive_last_flush_millis ?? 0} ms ago</div>
          <div><span class="muted">Archive compression</span> {((ops.archive_last_compression_ppm ?? 0) / 1_000_000).toFixed(2)}x</div>
        </div>
      </div>
    </div>

  {/if}
</div>

<style>
  @keyframes pulse { 0%, 100% { opacity: 0.4; } 50% { opacity: 0.2; } }
  .skeleton { height: 90px; opacity: 0.4; animation: pulse 1.5s infinite; }
  .ops-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(175px, 1fr)); gap: 12px; }
  .sparkline-card { display: flex; flex-direction: column; gap: 4px; }
  .sparkline { width: 100%; height: 24px; margin-top: 4px; }
  .sparkline path { fill: none; stroke: var(--blue, #58a6ff); stroke-width: 1.5; vector-effect: non-scaling-stroke; }
  .ops-sections { display: grid; gap: 16px; margin-top: 16px; }
  .section { padding: 16px; margin-top: 16px; }
  .section h3 { margin: 0 0 12px; font-size: 14px; }
  .check-grid { display: grid; gap: 10px; font-size: 13px; }
  .warn-text { color: var(--yellow, #d29922); }
  .ghost-link {
    font-size: 12px; color: var(--blue); text-decoration: none;
    border: 1px solid var(--border); padding: 4px 10px; border-radius: 4px;
  }
  .ghost-link:hover { background: var(--bg2); }
</style>
