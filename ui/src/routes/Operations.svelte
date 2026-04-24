<script>
  import { onMount } from 'svelte';

  let ops = $state(null);
  let loading = $state(true);
  let error = $state(null);

  onMount(async () => {
    try {
      const r = await fetch('/api/operations');
      if (!r.ok) throw new Error(await r.text());
      ops = await r.json();
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  });
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">System</p>
      <h2>Operations</h2>
    </div>
  </div>

  {#if loading}
    <div class="ops-grid">
      {#each [1, 2, 3, 4] as _}
        <div class="card skeleton"></div>
      {/each}
    </div>
  {:else if error}
    <div class="notice error">{error}</div>
  {:else if !ops}
    <div class="notice error">Core did not return an operations summary.</div>
  {:else}
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
        <strong>{ops.collectors_connected ?? 0} / {ops.collectors_total ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Observed subscriptions</span>
        <strong>{ops.observed_subscriptions ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Pending subscriptions</span>
        <strong>{ops.pending_subscriptions ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Silent subscriptions</span>
        <strong>{ops.silent_subscriptions ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Trusted remediations</span>
        <strong>{ops.remediation_rows_post_cutoff ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Event bus depth</span>
        <strong>{ops.event_bus_depth ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Event bus receivers</span>
        <strong>{ops.event_bus_receivers ?? 0}</strong>
      </div>
      <div class="card metric">
        <span>Archive lag</span>
        <strong>{ops.archive_lag_millis ?? 0} ms</strong>
      </div>
      <div class="card metric">
        <span>Archive buffer</span>
        <strong>{ops.archive_buffer_rows ?? 0}</strong>
      </div>
    </div>

    <div class="ops-sections">
      <div class="card">
        <h3 style="margin-bottom: 12px; font-size: 15px;">Rule engine activity</h3>
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
        <h3 style="margin-bottom: 12px; font-size: 15px;">Remediation outcomes</h3>
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
        <h3 style="margin-bottom: 12px; font-size: 15px;">Operator checklist</h3>
        <div class="check-grid">
          <div><span class="muted">Unassigned devices</span> {ops.unassigned_devices ?? 0}</div>
          <div><span class="muted">Trust cutoff</span> <code>{ops.cutoff_iso}</code></div>
          <div><span class="muted">Last archive flush</span> {ops.archive_last_flush_millis ?? 0} ms</div>
          <div><span class="muted">Archive compression</span> {((ops.archive_last_compression_ppm ?? 0) / 1000000).toFixed(2)}x</div>
          <div><span class="muted">Prometheus</span> <a href="/metrics" target="_blank" style="color: var(--blue);">Open metrics ↗</a></div>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  @keyframes pulse { 0%, 100% { opacity: 0.4; } 50% { opacity: 0.2; } }
  .skeleton { height: 90px; opacity: 0.4; animation: pulse 1.5s infinite; }
  .ops-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 12px; }
  .ops-sections { display: grid; gap: 16px; margin-top: 16px; }
  .check-grid { display: grid; gap: 10px; font-size: 13px; }
</style>
