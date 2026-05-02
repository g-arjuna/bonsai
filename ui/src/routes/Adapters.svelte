<script>
  import { onMount } from 'svelte';

  // ── State ────────────────────────────────────────────────────────────────────
  let adapters = $state([]);
  let auditEntries = $state([]);
  let loading = $state(true);
  let showForm = $state(false);
  let saving = $state(false);
  let testingName = $state('');
  let testResult = $state(null); // { name, success, message }

  const ADAPTER_TYPES = [
    { value: 'prometheus_remote_write', label: 'Prometheus Remote Write' },
    { value: 'splunk_hec',             label: 'Splunk HEC (Sprint 8)' },
    { value: 'elastic',                label: 'Elastic Ingest (Sprint 8)' },
    { value: 'servicenow_em',          label: 'ServiceNow Event Mgmt (Sprint 9)' },
  ];

  const emptyConfig = () => ({
    name: '',
    adapter_type: 'prometheus_remote_write',
    enabled: true,
    endpoint_url: '',
    credential_alias: '',
    flush_interval_secs: 30,
    environment_scope: [],
    extra: {},
  });

  let form = $state(emptyConfig());
  let editingName = $state(''); // '' means "new"
  let envScopeInput = $state('');

  // ── Load ─────────────────────────────────────────────────────────────────────
  async function load() {
    loading = true;
    try {
      const [aRes, auRes] = await Promise.all([
        fetch('/api/adapters'),
        fetch('/api/adapters/audit'),
      ]);
      if (aRes.ok)  adapters     = (await aRes.json()).adapters ?? [];
      if (auRes.ok) auditEntries = (await auRes.json()).entries ?? [];
    } catch (e) {
      console.error('adapter load error', e);
    } finally {
      loading = false;
    }
  }

  onMount(load);

  // ── Form helpers ─────────────────────────────────────────────────────────────
  function openNew() {
    form        = emptyConfig();
    editingName = '';
    envScopeInput = '';
    showForm    = true;
    testResult  = null;
  }

  function openEdit(adapter) {
    form        = { ...adapter.config };
    editingName = adapter.config.name;
    envScopeInput = (adapter.config.environment_scope ?? []).join(', ');
    showForm    = true;
    testResult  = null;
  }

  function cancelForm() {
    showForm   = false;
    testResult = null;
  }

  async function saveAdapter() {
    saving = true;
    form.environment_scope = envScopeInput
      .split(',')
      .map(s => s.trim())
      .filter(Boolean);

    try {
      const res = await fetch('/api/adapters', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ config: form }),
      });
      const data = await res.json();
      if (data.success) {
        showForm = false;
        await load();
      } else {
        alert('Save failed: ' + (data.error ?? 'unknown error'));
      }
    } catch (e) {
      alert('Save error: ' + e.message);
    } finally {
      saving = false;
    }
  }

  async function removeAdapter(name) {
    if (!confirm(`Remove adapter "${name}"?`)) return;
    try {
      const res = await fetch('/api/adapters/remove', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const data = await res.json();
      if (data.success) await load();
      else alert('Remove failed: ' + (data.error ?? 'unknown error'));
    } catch (e) {
      alert('Remove error: ' + e.message);
    }
  }

  async function testConnection(name) {
    testingName = name;
    testResult  = null;
    try {
      const res = await fetch('/api/adapters/test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const data = await res.json();
      testResult = { name, success: data.success, message: data.message };
    } catch (e) {
      testResult = { name, success: false, message: e.message };
    } finally {
      testingName = '';
    }
  }

  // ── Helpers ──────────────────────────────────────────────────────────────────
  function formatNs(ns) {
    if (!ns) return '—';
    const ms = Math.floor(ns / 1_000_000);
    const d  = new Date(ms);
    return d.toLocaleString();
  }

  function formatBytes(b) {
    if (!b) return '0 B';
    if (b < 1024) return b + ' B';
    if (b < 1048576) return (b / 1024).toFixed(1) + ' KB';
    return (b / 1048576).toFixed(1) + ' MB';
  }

  function adapterTypeLabel(t) {
    return ADAPTER_TYPES.find(a => a.value === t)?.label ?? t;
  }

  function topicHint(t) {
    const hints = {
      prometheus_remote_write: 'collector-side · raw telemetry counters',
      splunk_hec:              'core-side · detection events',
      elastic:                 'core-side · detection events',
      servicenow_em:           'core-side · detection events → em_event',
    };
    return hints[t] ?? '';
  }
</script>

<div class="workspace">
  <div class="workspace-header">
    <h1>Output Adapters</h1>
    <div class="header-actions">
      <button class="btn-secondary" onclick={load}>↺ Refresh</button>
      <button class="btn-primary" onclick={openNew}>+ Add adapter</button>
    </div>
  </div>

  <!-- Add / Edit form -->
  {#if showForm}
    <div class="adapter-form card">
      <h2>{editingName ? 'Edit adapter' : 'New adapter'}</h2>
      <div class="form-grid">
        <label>
          Name
          <input
            type="text"
            bind:value={form.name}
            placeholder="prom-lab"
            disabled={!!editingName}
          />
        </label>

        <label>
          Type
          <select bind:value={form.adapter_type}>
            {#each ADAPTER_TYPES as t}
              <option value={t.value}>{t.label}</option>
            {/each}
          </select>
          {#if topicHint(form.adapter_type)}
            <span class="hint">{topicHint(form.adapter_type)}</span>
          {/if}
        </label>

        <label class="span-2">
          Endpoint URL
          <input
            type="text"
            bind:value={form.endpoint_url}
            placeholder="http://prometheus:9090/api/v1/write"
          />
        </label>

        <label>
          Credential alias
          <input
            type="text"
            bind:value={form.credential_alias}
            placeholder="(leave empty for no auth)"
          />
        </label>

        <label>
          Flush interval (seconds)
          <input type="number" bind:value={form.flush_interval_secs} min="5" max="3600" />
        </label>

        <label class="span-2">
          Environment scope
          <input
            type="text"
            bind:value={envScopeInput}
            placeholder="data_center, service_provider  (empty = all)"
          />
        </label>

        {#if form.adapter_type === 'prometheus_remote_write'}
          <label>
            Job label (optional)
            <input
              type="text"
              placeholder="bonsai"
              value={form.extra?.job ?? ''}
              oninput={e => { form.extra = { ...form.extra, job: e.target.value }; }}
            />
          </label>
        {/if}

        <label class="checkbox-label">
          <input type="checkbox" bind:checked={form.enabled} />
          Enabled
        </label>
      </div>

      <div class="form-actions">
        <button class="btn-secondary" onclick={cancelForm}>Cancel</button>
        <button class="btn-primary" onclick={saveAdapter} disabled={saving || !form.name || !form.endpoint_url}>
          {saving ? 'Saving…' : 'Save'}
        </button>
      </div>
    </div>
  {/if}

  <!-- Test result banner -->
  {#if testResult}
    <div class="test-banner" class:test-ok={testResult.success} class:test-fail={!testResult.success}>
      <strong>{testResult.name}</strong>:
      {testResult.success ? '✓ ' : '✗ '}{testResult.message}
      <button class="banner-dismiss" onclick={() => testResult = null}>×</button>
    </div>
  {/if}

  <!-- Adapter list -->
  {#if loading}
    <div class="loading">Loading adapters…</div>
  {:else if adapters.length === 0}
    <div class="empty-state">
      <p>No output adapters configured.</p>
      <p>Add a <strong>Prometheus Remote Write</strong> adapter to start exporting interface
         telemetry to your Prometheus / Grafana stack.</p>
    </div>
  {:else}
    {#each adapters as a (a.config.name)}
      <div class="adapter-card card" class:disabled-card={!a.config.enabled}>
        <div class="adapter-header">
          <div class="adapter-title">
            <span class="adapter-status-dot" class:dot-enabled={a.config.enabled} class:dot-disabled={!a.config.enabled}></span>
            <strong>{a.config.name}</strong>
            <span class="adapter-type-badge">{adapterTypeLabel(a.config.type ?? a.config.adapter_type)}</span>
            {#if topicHint(a.config.adapter_type)}
              <span class="topic-hint">{topicHint(a.config.adapter_type)}</span>
            {/if}
          </div>
          <div class="adapter-actions">
            <button
              class="btn-sm"
              onclick={() => testConnection(a.config.name)}
              disabled={testingName === a.config.name}
            >
              {testingName === a.config.name ? 'Testing…' : 'Test'}
            </button>
            <button class="btn-sm" onclick={() => openEdit(a)}>Edit</button>
            <button class="btn-sm btn-danger" onclick={() => removeAdapter(a.config.name)}>Remove</button>
          </div>
        </div>

        <div class="adapter-meta">
          <span>URL: <code>{a.config.endpoint_url}</code></span>
          {#if a.config.credential_alias}
            <span>Credential: <code>{a.config.credential_alias}</code></span>
          {:else}
            <span class="dim">No auth</span>
          {/if}
          <span>Flush: {a.config.flush_interval_secs}s</span>
          {#if a.config.environment_scope?.length}
            <span>Scope: {a.config.environment_scope.join(', ')}</span>
          {:else}
            <span class="dim">All environments</span>
          {/if}
        </div>

        <!-- Runtime stats -->
        <div class="adapter-run-state">
          {#if a.state.last_push_at_ns}
            <span class:state-error={a.state.last_push_error}>
              Last push: {formatNs(a.state.last_push_at_ns)}
              · {a.state.last_push_events ?? 0} events
              · {formatBytes(a.state.last_push_bytes ?? 0)}
              {#if a.state.last_push_duration_ms}
                · {a.state.last_push_duration_ms}ms
              {/if}
            </span>
            {#if a.state.last_push_error}
              <span class="run-error">Error: {a.state.last_push_error}</span>
            {/if}
          {:else}
            <span class="dim">No push recorded yet — adapter starts on next server boot.</span>
          {/if}
          {#if a.state.total_events_pushed > 0}
            <span class="totals">
              Total: {a.state.total_events_pushed.toLocaleString()} events
              · {formatBytes(a.state.total_bytes_sent ?? 0)}
            </span>
          {/if}
        </div>
      </div>
    {/each}
  {/if}

  <!-- Audit log -->
  <section class="audit-section">
    <h2>Push Audit Log</h2>
    {#if auditEntries.length === 0}
      <p class="dim">No adapter push events recorded yet.</p>
    {:else}
      <table class="audit-table">
        <thead>
          <tr>
            <th>Time</th>
            <th>Adapter</th>
            <th>Outcome</th>
            <th>Events</th>
            <th>Bytes</th>
            <th>Error</th>
          </tr>
        </thead>
        <tbody>
          {#each auditEntries as entry}
            <tr class="audit-row" class:row-error={entry.outcome === 'error'}>
              <td>{formatNs(entry.timestamp_ns)}</td>
              <td><code>{entry.adapter}</code></td>
              <td>
                <span class="outcome-badge" class:outcome-ok={entry.outcome === 'success'} class:outcome-err={entry.outcome === 'error'}>
                  {entry.outcome}
                </span>
              </td>
              <td>{entry.events_pushed ?? '—'}</td>
              <td>{entry.bytes_sent ? formatBytes(entry.bytes_sent) : '—'}</td>
              <td class="error-cell">{entry.error ?? ''}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>
</div>

<style>
  .workspace {
    padding: 1.5rem;
    max-width: 1100px;
  }
  .workspace-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 1.5rem;
  }
  .workspace-header h1 { margin: 0; font-size: 1.4rem; }
  .header-actions { display: flex; gap: 0.5rem; }

  .card {
    background: var(--surface, #1a1a1a);
    border: 1px solid var(--border, #2a2a2a);
    border-radius: 6px;
    padding: 1rem;
    margin-bottom: 1rem;
  }

  /* Form */
  .adapter-form h2 { margin-top: 0; font-size: 1.1rem; }
  .form-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.75rem;
    margin-bottom: 1rem;
  }
  .form-grid label { display: flex; flex-direction: column; gap: 0.25rem; font-size: 0.85rem; }
  .form-grid input, .form-grid select {
    background: var(--input-bg, #111);
    border: 1px solid var(--border, #333);
    border-radius: 4px;
    color: inherit;
    padding: 0.35rem 0.5rem;
    font-size: 0.85rem;
  }
  .span-2 { grid-column: span 2; }
  .hint { font-size: 0.75rem; color: var(--muted, #888); margin-top: 2px; }
  .checkbox-label { flex-direction: row; align-items: center; gap: 0.5rem; }
  .form-actions { display: flex; justify-content: flex-end; gap: 0.5rem; }

  /* Test banner */
  .test-banner {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.6rem 1rem;
    border-radius: 4px;
    margin-bottom: 1rem;
    font-size: 0.88rem;
  }
  .test-ok   { background: rgba(34, 197, 94, 0.12); border: 1px solid rgba(34, 197, 94, 0.3); }
  .test-fail { background: rgba(239, 68, 68, 0.12);  border: 1px solid rgba(239, 68, 68, 0.3); }
  .banner-dismiss { margin-left: auto; background: none; border: none; cursor: pointer; color: inherit; font-size: 1.1rem; }

  /* Adapter card */
  .adapter-card { transition: opacity 0.2s; }
  .disabled-card { opacity: 0.6; }
  .adapter-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.6rem;
  }
  .adapter-title { display: flex; align-items: center; gap: 0.5rem; flex-wrap: wrap; }
  .adapter-status-dot {
    width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0;
  }
  .dot-enabled  { background: #22c55e; }
  .dot-disabled { background: #6b7280; }
  .adapter-type-badge {
    font-size: 0.72rem;
    background: var(--badge-bg, #2a2a2a);
    border: 1px solid var(--border, #333);
    border-radius: 3px;
    padding: 1px 6px;
  }
  .topic-hint { font-size: 0.72rem; color: var(--muted, #888); }
  .adapter-actions { display: flex; gap: 0.4rem; flex-shrink: 0; }
  .adapter-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    font-size: 0.8rem;
    color: var(--muted, #888);
    margin-bottom: 0.5rem;
  }
  .adapter-meta code { color: var(--text, inherit); font-size: 0.78rem; }
  .adapter-run-state {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.8rem;
    padding-top: 0.4rem;
    border-top: 1px solid var(--border, #2a2a2a);
  }
  .state-error { color: #ef4444; }
  .run-error   { color: #ef4444; font-size: 0.78rem; }
  .totals      { color: var(--muted, #888); font-size: 0.78rem; }

  /* Buttons */
  .btn-primary {
    background: var(--accent, #3b82f6);
    color: #fff;
    border: none;
    border-radius: 4px;
    padding: 0.4rem 0.9rem;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-secondary {
    background: var(--surface2, #222);
    color: inherit;
    border: 1px solid var(--border, #333);
    border-radius: 4px;
    padding: 0.4rem 0.9rem;
    cursor: pointer;
    font-size: 0.85rem;
  }
  .btn-sm {
    background: var(--surface2, #222);
    border: 1px solid var(--border, #333);
    border-radius: 3px;
    padding: 0.2rem 0.6rem;
    font-size: 0.78rem;
    cursor: pointer;
    color: inherit;
  }
  .btn-sm:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-danger { color: #ef4444; border-color: rgba(239,68,68,0.4); }

  /* Audit table */
  .audit-section { margin-top: 2rem; }
  .audit-section h2 { font-size: 1rem; margin-bottom: 0.75rem; }
  .audit-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.8rem;
  }
  .audit-table th {
    text-align: left;
    padding: 0.4rem 0.5rem;
    border-bottom: 1px solid var(--border, #333);
    color: var(--muted, #888);
    font-weight: 500;
  }
  .audit-row td {
    padding: 0.35rem 0.5rem;
    border-bottom: 1px solid var(--border-faint, #1f1f1f);
    vertical-align: top;
  }
  .audit-row:hover td { background: var(--hover, #1f1f1f); }
  .row-error td { color: #ef4444; }
  .error-cell { font-size: 0.75rem; max-width: 300px; word-break: break-all; }
  .outcome-badge {
    font-size: 0.72rem;
    border-radius: 3px;
    padding: 1px 5px;
  }
  .outcome-ok  { background: rgba(34,197,94,0.15);  color: #22c55e; }
  .outcome-err { background: rgba(239,68,68,0.15);  color: #ef4444; }

  /* Misc */
  .dim { color: var(--muted, #888); }
  .loading { padding: 2rem; text-align: center; color: var(--muted, #888); }
  .empty-state {
    padding: 2rem;
    text-align: center;
    color: var(--muted, #888);
    border: 1px dashed var(--border, #333);
    border-radius: 6px;
  }
</style>
