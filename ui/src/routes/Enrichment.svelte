<script>
  import { onMount } from 'svelte';

  let enrichers = $state([]);
  let loading = $state(true);
  let error = $state('');
  let message = $state('');
  let auditEntries = $state([]);
  let auditLoading = $state(false);

  // Form state for add/edit
  let showForm = $state(false);
  let saving = $state(false);
  let form = $state(emptyForm());

  function emptyForm() {
    return {
      name: '',
      enricher_type: 'netbox',
      enabled: true,
      base_url: '',
      credential_alias: '',
      poll_interval_secs: 3600,
      environment_scope: [],
      extra: {},
    };
  }

  async function load() {
    loading = true;
    try {
      const res = await fetch('/api/enrichment');
      if (!res.ok) throw new Error(await res.text());
      const body = await res.json();
      enrichers = body.enrichers || [];
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function save() {
    if (!form.name.trim() || !form.base_url.trim()) {
      error = 'Name and Base URL are required.';
      return;
    }
    saving = true;
    error = '';
    try {
      const res = await fetch('/api/enrichment', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ config: { ...form, poll_interval_secs: Number(form.poll_interval_secs) } }),
      });
      const body = await res.json();
      if (!body.success) throw new Error(body.error || 'save failed');
      message = `Enricher "${form.name}" saved.`;
      showForm = false;
      form = emptyForm();
      await load();
    } catch (e) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  async function remove(name) {
    if (!confirm(`Remove enricher "${name}"?`)) return;
    error = '';
    try {
      const res = await fetch('/api/enrichment/remove', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const body = await res.json();
      if (!body.success) throw new Error(body.error || 'remove failed');
      message = `Enricher "${name}" removed.`;
      await load();
    } catch (e) {
      error = e.message;
    }
  }

  async function testConnection(name) {
    error = '';
    message = `Testing connection for "${name}" ...`;
    try {
      const res = await fetch('/api/enrichment/test', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const body = await res.json();
      message = body.success
        ? `"${name}" reachable: ${body.message}`
        : `"${name}" unreachable: ${body.message}`;
    } catch (e) {
      error = e.message;
    }
  }

  async function runNow(name) {
    error = '';
    try {
      const res = await fetch('/api/enrichment/run', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const body = await res.json();
      if (!body.success) throw new Error(body.error || 'run failed');
      message = body.message;
      setTimeout(load, 1500);
    } catch (e) {
      error = e.message;
    }
  }

  async function loadAudit() {
    auditLoading = true;
    try {
      const res = await fetch('/api/enrichment/audit');
      if (!res.ok) throw new Error(await res.text());
      const body = await res.json();
      auditEntries = body.entries || [];
    } catch (e) {
      error = e.message;
    } finally {
      auditLoading = false;
    }
  }

  function editEnricher(entry) {
    form = { ...entry.config };
    showForm = true;
  }

  function fmtTs(ns) {
    if (!ns) return '—';
    return new Date(ns / 1e6).toLocaleString();
  }

  function fmtDuration(ms) {
    if (ms === undefined || ms === null) return '—';
    if (ms < 1000) return `${ms} ms`;
    return `${(ms / 1000).toFixed(1)} s`;
  }

  onMount(() => {
    load();
    loadAudit();
  });
</script>

<div class="view">
  <section class="workspace-header">
    <div>
      <p class="eyebrow">Enrichment</p>
      <h2>Graph enrichment integrations</h2>
      <p class="muted">Connect external CMDBs and IPAMs to write business context into the bonsai graph. All credentials are resolved from the vault — never entered here directly.</p>
    </div>
    <div class="workspace-switcher">
      <button onclick={load} class="ghost">Refresh</button>
      <button onclick={() => { form = emptyForm(); showForm = !showForm; }}>
        {showForm ? 'Cancel' : '+ Add enricher'}
      </button>
    </div>
  </section>

  {#if error}
    <div class="notice error">{error}</div>
  {/if}
  {#if message}
    <div class="notice success">{message}</div>
  {/if}

  {#if showForm}
    <section class="enrichment-form card">
      <p class="eyebrow">{form.name ? `Editing ${form.name}` : 'New enricher'}</p>
      <div class="form-grid">
        <label>
          Name (stable identifier)
          <input type="text" bind:value={form.name} placeholder="netbox" disabled={!!form._editing} />
        </label>
        <label>
          Type
          <select bind:value={form.enricher_type}>
            <option value="netbox">NetBox (IPAM/DCIM)</option>
            <option value="servicenow">ServiceNow CMDB</option>
            <option value="cli_scrape">CLI scrape (SSH)</option>
          </select>
        </label>
        <label>
          Base URL
          <input type="url" bind:value={form.base_url} placeholder="http://netbox:8000" />
        </label>
        <label>
          Credential alias
          <input type="text" bind:value={form.credential_alias} placeholder="netbox-token" />
          <small>Alias in the vault — go to Credentials to add it first.</small>
        </label>
        <label>
          Poll interval (seconds)
          <input type="number" bind:value={form.poll_interval_secs} min="0" placeholder="3600" />
          <small>0 = manual only</small>
        </label>
        <label class="checkbox-label">
          <input type="checkbox" bind:checked={form.enabled} />
          Enabled
        </label>
      </div>
      <div class="form-actions">
        <button class="ghost" onclick={() => showForm = false}>Cancel</button>
        <button onclick={save} disabled={saving}>{saving ? 'Saving…' : 'Save enricher'}</button>
      </div>
    </section>
  {/if}

  {#if loading}
    <p class="empty">Loading enrichers…</p>
  {:else if !enrichers.length}
    <div class="empty-state">
      <p class="empty">No enrichers configured yet.</p>
      <p class="muted">Add a NetBox or ServiceNow enricher to start writing business context into the graph. Make sure the credential alias exists in the vault before saving.</p>
    </div>
  {:else}
    <div class="enricher-list">
      {#each enrichers as entry}
        {@const cfg = entry.config}
        {@const st = entry.state}
        <article class="enricher-card card">
          <header class="enricher-header">
            <div class="enricher-title">
              <span class="badge {cfg.enabled ? 'healthy' : 'critical'}">{cfg.enabled ? 'enabled' : 'disabled'}</span>
              <strong>{cfg.name}</strong>
              <code class="enricher-type">{cfg.enricher_type}</code>
            </div>
            <div class="enricher-actions">
              <button class="ghost small" onclick={() => testConnection(cfg.name)}>Test connection</button>
              <button class="ghost small" onclick={() => runNow(cfg.name)} disabled={st.is_running}>
                {st.is_running ? 'Running…' : 'Run now'}
              </button>
              <button class="ghost small" onclick={() => editEnricher(entry)}>Edit</button>
              <button class="danger small" onclick={() => remove(cfg.name)}>Remove</button>
            </div>
          </header>

          <div class="enricher-meta">
            <div>
              <span class="label">URL</span>
              <code>{cfg.base_url}</code>
            </div>
            <div>
              <span class="label">Credential</span>
              <span>{cfg.credential_alias || '—'}</span>
            </div>
            <div>
              <span class="label">Poll interval</span>
              <span>{cfg.poll_interval_secs ? `${cfg.poll_interval_secs} s` : 'manual'}</span>
            </div>
            {#if cfg.environment_scope?.length}
              <div>
                <span class="label">Env scope</span>
                <span>{cfg.environment_scope.join(', ')}</span>
              </div>
            {/if}
          </div>

          <div class="enricher-run-state">
            {#if st.last_run_at_ns}
              <span class="label">Last run</span>
              <span>{fmtTs(st.last_run_at_ns)}</span>
              <span class="label">Duration</span>
              <span>{fmtDuration(st.last_run_duration_ms)}</span>
              <span class="label">Nodes touched</span>
              <span>{st.last_run_nodes_touched ?? '—'}</span>
              {#if st.last_run_error}
                <span class="label">Error</span>
                <span class="error-text">{st.last_run_error}</span>
              {/if}
              {#if st.last_run_warnings?.length}
                <div class="run-warnings">
                  {#each st.last_run_warnings as w}
                    <p class="warning">{w}</p>
                  {/each}
                </div>
              {/if}
            {:else}
              <span class="muted">Never run.</span>
            {/if}
          </div>
        </article>
      {/each}
    </div>
  {/if}

  <!-- Audit log section -->
  <section class="enrichment-audit">
    <div class="section-title">
      <h3>Enrichment audit log</h3>
      <button class="ghost small" onclick={loadAudit} disabled={auditLoading}>
        {auditLoading ? 'Loading…' : 'Refresh'}
      </button>
    </div>
    {#if !auditEntries.length}
      <p class="empty">No enrichment run events recorded yet.</p>
    {:else}
      <div class="audit-table">
        <div class="audit-header-row">
          <span>Time</span>
          <span>Enricher</span>
          <span>Outcome</span>
          <span>Nodes touched</span>
          <span>Error</span>
        </div>
        {#each auditEntries as entry}
          <div class="audit-row {entry.outcome === 'success' || entry.outcome?.endsWith('success') ? 'ok' : 'fail'}">
            <span>{fmtTs(entry.timestamp_ns)}</span>
            <span>{entry.enricher}</span>
            <span><span class="badge {entry.outcome?.includes('success') ? 'healthy' : 'critical'}">{entry.outcome}</span></span>
            <span>{entry.nodes_touched ?? '—'}</span>
            <span class="error-text">{entry.error ?? ''}</span>
          </div>
        {/each}
      </div>
    {/if}
  </section>
</div>
