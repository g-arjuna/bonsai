<script lang="ts">
  import { onMount } from 'svelte';
  import { ChevronRight, CheckCircle, XCircle, Clock, Server, Activity, Settings } from 'lucide-svelte';

  interface HAStatus {
    mode: string;
    node_id: string;
    leader_state: string;
    leader_id?: string;
    is_leader: boolean;
    etcd_connected: boolean;
    etcd_endpoints?: string;
  }

  interface HASettings {
    mode: string;
    node_id: string;
    etcd_endpoints: string;
    election_ttl_secs: number;
    config_prefix: string;
  }

  let status: HAStatus | null = null;
  let settings: HASettings | null = null;
  let loading = true;
  let error: string | null = null;
  let editingSettings = false;

  async function loadStatus() {
    try {
      const res = await fetch('/api/ha/status');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      status = await res.json();
    } catch (e) {
      error = `Failed to load HA status: ${e}`;
    }
  }

  async function loadSettings() {
    try {
      const res = await fetch('/api/ha/settings');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      settings = await res.json();
    } catch (e) {
      error = `Failed to load HA settings: ${e}`;
    }
  }

  async function saveSettings() {
    if (!settings) return;
    try {
      const res = await fetch('/api/ha/settings', {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(settings)
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      settings = await res.json();
      editingSettings = false;
      await loadStatus();
    } catch (e) {
      error = `Failed to save HA settings: ${e}`;
    }
  }

  onMount(async () => {
    loading = true;
    await Promise.all([loadStatus(), loadSettings()]);
    loading = false;
  });

  function getLeaderStatusColor() {
    if (!status) return 'var(--state-info-bg)';
    if (status.is_leader) return 'var(--state-healthy-bg)';
    if (status.leader_state.startsWith('follower')) return 'var(--state-degraded-bg)';
    return 'var(--state-failed-bg)';
  }

  function getLeaderStatusIcon() {
    if (!status) return Clock;
    if (status.is_leader) return CheckCircle;
    if (status.leader_state.startsWith('follower')) return Activity;
    return XCircle;
  }
</script>

<div class="ha-page">
  <header class="ha-header">
    <h1>High Availability</h1>
    <p class="subtitle">Configure and monitor HA cluster status</p>
  </header>

  {#if loading}
    <div class="loading">Loading HA status...</div>
  {:else if error}
    <div class="error">{error}</div>
  {/if}

  {#if status}
    <section class="status-card">
      <h2>Cluster Status</h2>
      <div class="status-grid">
        <div class="status-item">
          <span class="label">Mode</span>
          <span class="value">{status.mode}</span>
        </div>
        <div class="status-item">
          <span class="label">Node ID</span>
          <span class="value">{status.node_id}</span>
        </div>
        <div class="status-item">
          <span class="label">Leader State</span>
          <span class="value" style:color={getLeaderStatusColor()}>
            <svelte:component this={getLeaderStatusIcon()} size={16} style="vertical-align: middle; margin-right: 4px;" />
            {status.leader_state}
          </span>
        </div>
        {#if status.leader_id}
          <div class="status-item">
            <span class="label">Leader ID</span>
            <span class="value">{status.leader_id}</span>
          </div>
        {/if}
        <div class="status-item">
          <span class="label">etcd Connected</span>
          <span class="value">
            {#if status.etcd_connected}
              <CheckCircle size={16} style="color: var(--state-healthy-border);" />
            {:else}
              <XCircle size={16} style="color: var(--state-failed-border);" />
            {/if}
          </span>
        </div>
        {#if status.etcd_endpoints}
          <div class="status-item">
            <span class="label">etcd Endpoints</span>
            <span class="value" style="font-family: monospace; font-size: 0.9em;">{status.etcd_endpoints}</span>
          </div>
        {/if}
      </div>
    </section>
  {/if}

  {#if settings}
    <section class="settings-card">
      <div class="settings-header">
        <h2>HA Settings</h2>
        {#if !editingSettings}
          <button class="btn-secondary" on:click={() => editingSettings = true}>
            <Settings size={16} style="margin-right: 4px;" />
            Edit
          </button>
        {:else}
          <div class="edit-actions">
            <button class="btn-secondary" on:click={() => editingSettings = false}>Cancel</button>
            <button class="btn-primary" on:click={saveSettings}>Save</button>
          </div>
        {/if}
      </div>

      <div class="settings-form">
        <div class="form-group">
          <label>HA Mode</label>
          <select bind:value={settings.mode} disabled={!editingSettings}>
            <option value="standalone">Standalone</option>
            <option value="cluster">Cluster</option>
          </select>
          <small>⚠ Changing mode requires process restart</small>
        </div>

        <div class="form-group">
          <label>Node ID</label>
          <input type="text" bind:value={settings.node_id} disabled={!editingSettings} />
          <small>Unique identifier for this node in the cluster</small>
        </div>

        <div class="form-group">
          <label>etcd Endpoints</label>
          <input type="text" bind:value={settings.etcd_endpoints} disabled={!editingSettings} />
          <small>Comma-separated etcd endpoint URLs (e.g., http://etcd-1:2379,http://etcd-2:2379)</small>
        </div>

        <div class="form-group">
          <label>Election TTL (seconds)</label>
          <input type="number" bind:value={settings.election_ttl_secs} disabled={!editingSettings} />
          <small>Leader election lease TTL - lower values = faster failover but more etcd traffic</small>
        </div>

        <div class="form-group">
          <label>Config Prefix</label>
          <input type="text" bind:value={settings.config_prefix} disabled={!editingSettings} />
          <small>etcd key prefix for config replication (e.g., /bonsai/config)</small>
        </div>
      </div>
    </section>
  {/if}

  <section class="troubleshoot-card">
    <h2>Troubleshooting Wizard</h2>
    <div class="wizard-content">
      <p>Interactive troubleshooting guide coming soon.</p>
      <ul class="troubleshoot-steps">
        <li>Check etcd cluster health</li>
        <li>Verify network connectivity between nodes</li>
        <li>Review bonsai logs for election errors</li>
        <li>Validate configuration consistency across nodes</li>
      </ul>
    </div>
  </section>
</div>

<style>
  .ha-page {
    padding: 2rem;
    max-width: 1200px;
    margin: 0 auto;
  }

  .ha-header {
    margin-bottom: 2rem;
  }

  .ha-header h1 {
    margin: 0 0 0.5rem 0;
    font-size: 2rem;
    color: var(--text-primary);
  }

  .subtitle {
    margin: 0;
    color: var(--text-secondary);
  }

  .loading, .error {
    padding: 2rem;
    text-align: center;
    color: var(--text-secondary);
  }

  .error {
    color: var(--state-failed-border);
    background: var(--state-failed-bg);
    border-radius: 8px;
  }

  .status-card, .settings-card, .troubleshoot-card {
    background: var(--surface2);
    border-radius: 12px;
    padding: 1.5rem;
    margin-bottom: 1.5rem;
    border: 1px solid var(--border);
  }

  .status-card h2, .settings-card h2, .troubleshoot-card h2 {
    margin: 0 0 1rem 0;
    font-size: 1.25rem;
    color: var(--text-primary);
  }

  .status-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(250px, 1fr));
    gap: 1rem;
  }

  .status-item {
    padding: 1rem;
    background: var(--surface);
    border-radius: 8px;
    border: 1px solid var(--border);
  }

  .status-item .label {
    display: block;
    font-size: 0.875rem;
    color: var(--text-secondary);
    margin-bottom: 0.5rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .status-item .value {
    display: block;
    font-size: 1rem;
    color: var(--text-primary);
    font-weight: 500;
  }

  .settings-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1.5rem;
  }

  .edit-actions {
    display: flex;
    gap: 0.5rem;
  }

  .settings-form {
    display: grid;
    gap: 1.5rem;
  }

  .form-group {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .form-group label {
    font-size: 0.875rem;
    color: var(--text-primary);
    font-weight: 500;
  }

  .form-group input, .form-group select {
    padding: 0.5rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface);
    color: var(--text-primary);
    font-family: inherit;
  }

  .form-group input:disabled, .form-group select:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .form-group small {
    font-size: 0.75rem;
    color: var(--text-secondary);
  }

  .wizard-content {
    padding: 1rem;
  }

  .troubleshoot-steps {
    margin: 1rem 0 0 1.5rem;
    color: var(--text-secondary);
  }

  .troubleshoot-steps li {
    margin-bottom: 0.5rem;
  }

  .btn-primary, .btn-secondary {
    padding: 0.5rem 1rem;
    border-radius: 6px;
    font-size: 0.875rem;
    font-weight: 500;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
  }

  .btn-primary {
    background: var(--accent-primary);
    color: white;
    border: none;
  }

  .btn-secondary {
    background: var(--accent-muted);
    color: var(--text-primary);
    border: 1px solid var(--border);
  }
</style>
