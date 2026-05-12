<script>
  import { onMount } from 'svelte';

  let ops = $state(null);
  let collectors = $state(null);
  let subscriptions = $state(null);
  let dailyCheck = $state(null);
  let weeklyTrend = $state(null);
  let loading = $state(true);
  let error = $state(null);

  // Ring buffer — last 16 samples at 5s interval ≈ 80s of history
  const SPARKLINE_MAX = 16;
  let rssSamples     = $state([]);
  let archiveSamples = $state([]);
  let graphSamples   = $state([]);
  let busSamples     = $state([]);

  onMount(() => {
    fetchAll();
    const poll = setInterval(fetchAll, 5_000);
    return () => clearInterval(poll);
  });

  async function fetchAll() {
    try {
      const [opsRes, collRes, topoRes, dcRes, wtRes] = await Promise.all([
        fetch('/api/operations'),
        fetch('/api/assignment/status'),
        fetch('/api/topology'),
        fetch('/api/operations/daily-check'),
        fetch('/api/operations/weekly-trend'),
      ]);
      if (!opsRes.ok) throw new Error(await opsRes.text());
      ops = await opsRes.json();
      if (collRes.ok) collectors = await collRes.json();
      if (topoRes.ok) {
        const topo = await topoRes.json();
        subscriptions = topo.devices ?? [];
      }
      if (dcRes.ok) dailyCheck = await dcRes.json();
      if (wtRes.ok) weeklyTrend = await wtRes.json();
      rssSamples     = [...rssSamples,     ops.rss_bytes           ?? 0].slice(-SPARKLINE_MAX);
      archiveSamples = [...archiveSamples, ops.archive_disk_bytes  ?? 0].slice(-SPARKLINE_MAX);
      graphSamples   = [...graphSamples,   ops.graph_disk_bytes    ?? 0].slice(-SPARKLINE_MAX);
      busSamples     = [...busSamples,     ops.event_bus_depth     ?? 0].slice(-SPARKLINE_MAX);
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
    return 'M' + samples.map((v, i) =>
      `${(i / (samples.length - 1)) * w},${h - (v / max) * h}`
    ).join(' L');
  }

  function tileState(value, warnThreshold, critThreshold) {
    if (critThreshold != null && value >= critThreshold) return 'failed';
    if (warnThreshold != null && value >= warnThreshold) return 'degraded';
    return 'healthy';
  }

  function tc(value, w, c) {
    return 'tile tile-' + tileState(value, w, c);
  }

  function subTileClass(o) {
    if ((o?.silent_subscriptions ?? 0) > 0) return 'tile tile-failed';
    if ((o?.pending_subscriptions ?? 0) > 0) return 'tile tile-degraded';
    return 'tile tile-healthy';
  }

  function subBadgeClass(status) {
    if (status === 'observed') return 'healthy';
    if (status === 'pending')  return 'warn';
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

  function fmtMB(bytes) {
    return Math.round((bytes ?? 0) / 1024 / 1024) + ' MB';
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">System</p>
      <h2>Operations</h2>
    </div>
    <a href="/metrics" target="_blank" class="ghost-link">Prometheus metrics ↗</a>
  </div>

  {#if loading}
    <div class="tile-grid">
      {#each [1, 2, 3, 4, 5, 6] as _}
        <div class="tile skeleton-tile"></div>
      {/each}
    </div>
  {:else if error}
    <div class="notice error">{error}</div>
  {:else if !ops}
    <div class="notice error">Core did not return an operations summary.</div>
  {:else}

    <!-- ── Six primary tiles ─────────────────────────────────────────────── -->
    <div class="tile-grid">
      <div class="{tc(ops.event_bus_depth ?? 0, 500, 900)}">
        <span class="tile-label">Event bus</span>
        <strong class="tile-value">{ops.event_bus_depth ?? 0}</strong>
        <span class="tile-sub">{ops.event_bus_receivers ?? 0} receivers</span>
        {#if busSamples.length > 1}
          <svg class="sparkline" viewBox="0 0 100 20" preserveAspectRatio="none">
            <path d={sparklinePath(busSamples, 100, 20)} />
          </svg>
        {/if}
      </div>

      <div class="{tc(ops.archive_lag_millis ?? 0, 2000, 10000)}">
        <span class="tile-label">Archive lag</span>
        <strong class="tile-value">{ops.archive_lag_millis ?? 0} ms</strong>
        <span class="tile-sub">{ops.archive_buffer_rows ?? 0} rows buffered</span>
      </div>

      <div class="{tc(ops.memory_rss_pct_of_budget ?? 0, 70, 90)}">
        <span class="tile-label">RSS memory</span>
        <strong class="tile-value">{fmtMB(ops.rss_bytes)}</strong>
        {#if (ops.memory_budget_bytes ?? 0) > 0}
          <span class="tile-sub">{(ops.memory_rss_pct_of_budget ?? 0).toFixed(0)}% of {fmtMB(ops.memory_budget_bytes)} budget</span>
        {:else}
          <span class="tile-sub">no budget set</span>
        {/if}
        {#if rssSamples.length > 1}
          <svg class="sparkline" viewBox="0 0 100 20" preserveAspectRatio="none">
            <path d={sparklinePath(rssSamples, 100, 20)} />
          </svg>
        {/if}
      </div>

      <div class="{tc(ops.archive_disk_pct ?? 0, 70, 90)}">
        <span class="tile-label">Archive on disk</span>
        <strong class="tile-value">{fmtMB(ops.archive_disk_bytes)}</strong>
        {#if (ops.archive_disk_pct ?? 0) > 0}
          <span class="tile-sub">{ops.archive_disk_pct}% of quota</span>
        {:else}
          <span class="tile-sub">no quota set</span>
        {/if}
        {#if archiveSamples.length > 1}
          <svg class="sparkline" viewBox="0 0 100 20" preserveAspectRatio="none">
            <path d={sparklinePath(archiveSamples, 100, 20)} />
          </svg>
        {/if}
      </div>

      <div class="tile tile-healthy">
        <span class="tile-label">Detections</span>
        <strong class="tile-value">{ops.detection_events ?? 0}</strong>
        <span class="tile-sub">{ops.state_change_events ?? 0} state changes</span>
      </div>

      <div class="{subTileClass(ops)}">
        <span class="tile-label">Subscriptions</span>
        <strong class="tile-value">{ops.observed_subscriptions ?? 0}</strong>
        <span class="tile-sub">
          {#if (ops.silent_subscriptions ?? 0) > 0}
            {ops.silent_subscriptions} silent
          {:else if (ops.pending_subscriptions ?? 0) > 0}
            {ops.pending_subscriptions} pending
          {:else}
            all observed
          {/if}
        </span>
      </div>
    </div>

    <!-- ── Counter ingest mode ───────────────────────────────────────────── -->
    {#if ops.counter_mode}
      <div class="section-card counter-card">
        <span class="section-eyebrow">Counter ingest</span>
        <div class="counter-body">
          <span class="badge {ops.counter_mode === 'summary' ? 'info' : ops.counter_mode === 'raw' ? 'warn' : 'healthy'}">
            {ops.counter_mode}
          </span>
          <span class="counter-desc">
            {#if ops.counter_mode === 'summary'}
              Aggregating into <strong>{ops.counter_window_secs ?? 60}s</strong> windows — rate-of-change computed per window.
            {:else if ops.counter_mode === 'raw'}
              Forwarding raw counter samples at full gNMI cadence.
            {:else}
              Debouncing with a <strong>{ops.counter_debounce_secs ?? 10}s</strong> minimum interval per interface.
            {/if}
          </span>
        </div>
      </div>
    {/if}

    <!-- ── Secondary: device/collector counts ───────────────────────────── -->
    <div class="kv-row">
      <div class="kv">
        <span>Devices enabled</span>
        <strong>{ops.enabled_device_count ?? 0} / {ops.device_count ?? 0}</strong>
      </div>
      <div class="kv">
        <span>Collectors</span>
        <strong class="{(ops.collectors_connected ?? 0) === 0 && (ops.collectors_total ?? 0) > 0 ? 'state-failed' : ''}">
          {ops.collectors_connected ?? 0} / {ops.collectors_total ?? 0} connected
        </strong>
      </div>
      <div class="kv">
        <span>Trusted remediations</span>
        <strong>{ops.remediation_rows_post_cutoff ?? 0}</strong>
      </div>
      <div class="kv">
        <span>Archive last flush</span>
        <strong>{ops.archive_last_flush_millis ?? 0} ms ago</strong>
      </div>
      <div class="kv">
        <span>Trust cutoff</span>
        <strong><code>{ops.cutoff_iso ?? '—'}</code></strong>
      </div>
      {#if (ops.unassigned_devices ?? 0) > 0}
        <div class="kv kv-warn">
          <span>Unassigned devices</span>
          <strong>{ops.unassigned_devices}</strong>
        </div>
      {/if}
    </div>

    <!-- ── Collector health ──────────────────────────────────────────────── -->
    {#if collectors?.collectors?.length}
      <div class="section-card">
        <h3>Collector health</h3>
        <table>
          <thead>
            <tr>
              <th>Collector</th>
              <th>Status</th>
              <th>Devices</th>
              <th>Subscriptions</th>
              <th>Queue</th>
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
                  <span class="badge healthy">{c.observed_subscriptions ?? 0} obs</span>
                  {#if (c.pending_subscriptions ?? 0) > 0}
                    <span class="badge warn">{c.pending_subscriptions} pend</span>
                  {/if}
                  {#if (c.silent_subscriptions ?? 0) > 0}
                    <span class="badge critical">{c.silent_subscriptions} silent</span>
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

    <!-- ── Subscription health per device ───────────────────────────────── -->
    {#if subscriptions?.length}
      <div class="section-card">
        <h3>Subscription health</h3>
        <table>
          <thead>
            <tr><th>Device</th><th>Health</th><th>BGP peers</th><th>Role · Site</th></tr>
          </thead>
          <tbody>
            {#each subscriptions as dev}
              <tr>
                <td>
                  <strong>{dev.hostname || dev.address}</strong>
                  {#if dev.hostname}<br><span class="mono-small">{dev.address}</span>{/if}
                </td>
                <td><span class="badge {dev.health}">{dev.health}</span></td>
                <td>
                  {#if dev.bgp?.length}
                    {dev.bgp.filter(b => b.state === 'established').length}/{dev.bgp.length} established
                  {:else}
                    <span class="muted">—</span>
                  {/if}
                </td>
                <td class="muted">{dev.role || 'unknown'} · {dev.site || '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}

    <!-- ── Rule engine + Remediation outcomes ───────────────────────────── -->
    <div class="two-col">
      <div class="section-card">
        <h3>Rule engine activity</h3>
        {#if Object.keys(ops.rule_distribution ?? {}).length === 0}
          <div class="empty">No rule activity recorded yet.</div>
        {:else}
          <table>
            <thead><tr><th>Rule</th><th>Detections</th></tr></thead>
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

      <div class="section-card">
        <h3>Remediation outcomes</h3>
        {#if Object.keys(ops.status_distribution_post_cutoff ?? {}).length === 0}
          <div class="empty">No outcomes recorded yet.</div>
        {:else}
          <table>
            <thead><tr><th>Status</th><th>Count</th></tr></thead>
            <tbody>
              {#each Object.entries(ops.status_distribution_post_cutoff ?? {}) as [status, count]}
                <tr>
                  <td>
                    <span class="badge {status === 'succeeded' ? 'healthy' : status === 'failed' ? 'critical' : 'info'}">
                      {status}
                    </span>
                  </td>
                  <td>{count}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </div>
    </div>

    <!-- ── Graph DB ──────────────────────────────────────────────────────── -->
    <div class="section-card" style="margin-top:0">
      <h3>Graph DB on disk</h3>
      <div class="kv-row">
        <div class="kv">
          <span>Size</span>
          <strong>{fmtMB(ops.graph_disk_bytes)}</strong>
        </div>
        {#if (ops.graph_disk_pct ?? 0) > 0}
          <div class="kv">
            <span>Quota</span>
            <strong>{ops.graph_disk_pct}%</strong>
          </div>
        {/if}
        <div class="kv">
          <span>Compression</span>
          <strong>{((ops.archive_last_compression_ppm ?? 0) / 1_000_000).toFixed(2)}x</strong>
        </div>
      </div>
    </div>

    <!-- ── Driver results (daily check) ─────────────────────────────────── -->
    {#if dailyCheck}
      <div class="section-card">
        <div style="display:flex; align-items:center; gap:10px; margin-bottom:12px;">
          <h3 style="margin-bottom:0">Driver Results</h3>
          <span class="badge {dailyCheck.status === 'pass' ? 'healthy' : dailyCheck.status === 'fail' ? 'critical' : dailyCheck.status === 'pass_with_caveats' ? 'warn' : 'info'}">
            {dailyCheck.status}
          </span>
        </div>
        <div class="kv-row" style="margin-bottom:12px;">
          <div class="kv">
            <span>Pass</span>
            <strong style="color:var(--state-healthy)">{dailyCheck.counts?.pass ?? 0}</strong>
          </div>
          <div class="kv">
            <span>Fail</span>
            <strong style={dailyCheck.counts?.fail > 0 ? 'color:var(--state-failed)' : ''}>{dailyCheck.counts?.fail ?? 0}</strong>
          </div>
          <div class="kv">
            <span>Prereq missing</span>
            <strong style="color:var(--state-degraded)">{dailyCheck.counts?.prereq_missing ?? 0}</strong>
          </div>
          <div class="kv">
            <span>Skip</span>
            <strong>{dailyCheck.counts?.skip ?? 0}</strong>
          </div>
        </div>
        {#if dailyCheck.checks?.length}
          <table>
            <thead><tr><th>Check</th><th>Status</th><th>Summary</th></tr></thead>
            <tbody>
              {#each dailyCheck.checks as check}
                <tr>
                  <td><code>{check.name}</code></td>
                  <td>
                    <span class="badge {check.status === 'pass' ? 'healthy' : check.status === 'fail' ? 'critical' : check.status === 'prereq_missing' ? 'warn' : 'info'}">
                      {check.status}
                    </span>
                  </td>
                  <td class="muted">{check.summary || '—'}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {:else}
          <div class="empty">No driver result files found.</div>
        {/if}
      </div>
    {/if}

    <!-- ── 7-Day Trend ──────────────────────────────────────────────────── -->
    {#if weeklyTrend?.days?.length}
      <div class="section-card">
        <h3 style="margin-bottom:12px">7-Day Trend</h3>
        <table>
          <thead>
            <tr>
              <th>Date</th>
              <th>Status</th>
              <th style="text-align:right">Pass</th>
              <th style="text-align:right">Fail</th>
              <th style="text-align:right">Skip</th>
              <th style="text-align:right">Prereq</th>
            </tr>
          </thead>
          <tbody>
            {#each weeklyTrend.days as day}
              <tr>
                <td><code>{day.date || '—'}</code></td>
                <td>
                  <span class="badge {day.status === 'pass' ? 'healthy' : day.status === 'fail' ? 'critical' : day.status === 'pass_with_caveats' ? 'warn' : 'info'}">
                    {day.status}
                  </span>
                </td>
                <td style="text-align:right; color:var(--state-healthy)">{day.pass}</td>
                <td style="text-align:right; {day.fail > 0 ? 'color:var(--state-failed)' : ''}">{day.fail}</td>
                <td style="text-align:right" class="muted">{day.skip}</td>
                <td style="text-align:right; color:var(--state-degraded)">{day.prereq_missing}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}

  {/if}
</div>

<style>
  /* ── Skeleton ────────────────────────────────────────────────────────────── */
  @keyframes pulse { 0%, 100% { opacity: 0.35; } 50% { opacity: 0.15; } }
  .skeleton-tile {
    height: 96px;
    animation: pulse 1.5s infinite;
    background: var(--bg-surface);
  }

  /* ── Primary tile grid (3×2) ─────────────────────────────────────────────── */
  .tile-grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 10px;
    margin-bottom: 14px;
  }

  .tile {
    padding: var(--card-pad);
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    gap: 3px;
    position: relative;
    overflow: hidden;
    transition: border-color var(--duration-instant) var(--ease-out);
  }

  /* Coloured left-edge per state */
  .tile::before {
    content: '';
    position: absolute;
    left: 0; top: 0; bottom: 0;
    width: 3px;
    border-radius: 8px 0 0 8px;
  }
  .tile-healthy::before  { background: var(--state-healthy); }
  .tile-degraded::before { background: var(--state-degraded); }
  .tile-failed::before   { background: var(--state-failed); }

  .tile-label {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.07em;
    text-transform: uppercase;
    color: var(--text-tertiary);
  }

  .tile-value {
    font-size: 1.75rem;
    font-weight: 700;
    letter-spacing: var(--tracking-display);
    line-height: var(--leading-display);
    color: var(--text-primary);
  }

  .tile-failed  .tile-value  { color: var(--state-failed); }
  .tile-degraded .tile-value { color: var(--state-degraded); }

  .tile-sub {
    font-size: var(--text-small);
    color: var(--text-secondary);
  }

  .sparkline {
    width: 100%;
    height: 20px;
    margin-top: 6px;
    flex-shrink: 0;
  }
  .sparkline path {
    fill: none;
    stroke: var(--accent-primary);
    stroke-width: 1.5;
    vector-effect: non-scaling-stroke;
    opacity: 0.6;
  }

  /* ── Counter ingest ──────────────────────────────────────────────────────── */
  .counter-card { margin-bottom: 14px; }
  .counter-body {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 6px;
    flex-wrap: wrap;
  }
  .counter-desc {
    font-size: var(--text-small);
    color: var(--text-secondary);
    line-height: var(--leading-body);
  }

  /* ── KV row ──────────────────────────────────────────────────────────────── */
  .kv-row {
    display: flex;
    flex-wrap: wrap;
    gap: 10px 24px;
    padding: 12px var(--card-pad);
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    margin-bottom: 14px;
  }

  .kv {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 120px;
  }

  .kv span {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-tertiary);
  }

  .kv strong {
    font-size: var(--text-body);
    font-weight: 600;
    color: var(--text-primary);
  }

  .kv-warn strong { color: var(--state-degraded); }

  .state-failed { color: var(--state-failed); }

  /* ── Section cards ───────────────────────────────────────────────────────── */
  .section-card {
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: 6px;
    padding: var(--card-pad);
    margin-bottom: 14px;
  }

  .section-card h3 {
    font-size: var(--text-heading-2);
    font-weight: 600;
    letter-spacing: var(--tracking-display);
    margin-bottom: 12px;
    color: var(--text-primary);
  }

  .section-eyebrow {
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-tertiary);
  }

  /* ── Two-col layout ──────────────────────────────────────────────────────── */
  .two-col {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px;
    margin-bottom: 14px;
  }

  .two-col .section-card { margin-bottom: 0; }

  /* ── Helpers ─────────────────────────────────────────────────────────────── */
  .mono-small {
    font-family: var(--font-mono);
    font-size: var(--text-mono-sm);
    color: var(--text-tertiary);
  }

  .ghost-link {
    font-size: var(--text-small);
    color: var(--accent-primary);
    text-decoration: none;
    border: 1px solid var(--border-subtle);
    padding: 5px 10px;
    border-radius: 5px;
    transition: background var(--duration-instant) var(--ease-out);
    align-self: flex-start;
    margin-top: 6px;
  }

  .ghost-link:hover { background: var(--bg-glass); }

  @media (max-width: 900px) {
    .tile-grid { grid-template-columns: repeat(2, 1fr); }
    .two-col   { grid-template-columns: 1fr; }
  }
  @media (max-width: 500px) {
    .tile-grid { grid-template-columns: 1fr; }
  }
</style>
