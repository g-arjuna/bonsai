<script>
  import { onMount } from 'svelte';

  // ── Tab state ─────────────────────────────────────────────────────────────────
  let tab = $state('output');   // 'output' | 'enrichment'

  // ── Output Adapters ──────────────────────────────────────────────────────────
  let adapters      = $state([]);
  let adapterAudit  = $state([]);
  let adapterLoading = $state(true);
  let showAdapterForm = $state(false);
  let savingAdapter   = $state(false);
  let testingAdapter  = $state('');
  let adapterTestResult = $state(null); // { name, success, message }
  let editingAdapterName = $state('');

  const ADAPTER_TYPES = [
    { value: 'prometheus_remote_write', label: 'Prometheus Remote Write',
      hint: 'collector-side · raw telemetry counters → TSDB',
      cssVar: '--vendor-prometheus',
      defaultScheme: 'http', defaultHost: 'localhost', defaultPort: '9090', defaultPath: '/api/v1/write' },
    { value: 'splunk_hec',             label: 'Splunk HEC',
      hint: 'core-side · detection events → Splunk index',
      cssVar: '--vendor-splunk',
      defaultScheme: 'https', defaultHost: 'splunk', defaultPort: '8088', defaultPath: '' },
    { value: 'elastic',                label: 'Elasticsearch Bulk API',
      hint: 'core-side · detection events → ECS documents',
      cssVar: '--vendor-elastic',
      defaultScheme: 'http', defaultHost: 'elasticsearch', defaultPort: '9200', defaultPath: '' },
    { value: 'servicenow_em',          label: 'ServiceNow Event Mgmt',
      hint: 'core-side · detection events → em_event table',
      cssVar: '--vendor-servicenow',
      defaultScheme: 'https', defaultHost: 'instance.service-now.com', defaultPort: '443', defaultPath: '' },
  ];

  // ── Port / URL decomposition helpers ─────────────────────────────────────────
  const SCHEME_DEFAULT_PORTS = { http: '80', https: '443' };

  function parseEndpointUrl(url) {
    if (!url) return { scheme: 'http', host: '', port: '', path: '' };
    try {
      const u = new URL(url);
      const port = u.port || (u.protocol === 'https:' ? '443' : '80');
      return { scheme: u.protocol.replace(':', ''), host: u.hostname, port, path: u.pathname === '/' ? '' : u.pathname };
    } catch {
      return { scheme: 'http', host: url, port: '', path: '' };
    }
  }

  function composeEndpointUrl(scheme, host, port, path) {
    if (!host) return '';
    const defaultPort = SCHEME_DEFAULT_PORTS[scheme] ?? '';
    const portPart = (port && port !== defaultPort) ? `:${port}` : '';
    const pathPart = path ? (path.startsWith('/') ? path : '/' + path) : '';
    return `${scheme}://${host}${portPart}${pathPart}`;
  }

  function adapterTypeMeta(type) {
    return ADAPTER_TYPES.find(t => t.value === type) ?? ADAPTER_TYPES[0];
  }

  function emptyAdapter() {
    return {
      name: '', adapter_type: 'prometheus_remote_write',
      enabled: true, endpoint_url: '', credential_alias: '',
      flush_interval_secs: 30, environment_scope: [], extra: {},
    };
  }

  let adapterForm      = $state(emptyAdapter());
  let adapterEnvInput  = $state('');
  let adapterScheme    = $state('http');
  let adapterHost      = $state('');
  let adapterPort      = $state('9090');
  let adapterPath      = $state('/api/v1/write');

  function syncAdapterUrlFromParts() {
    adapterForm.endpoint_url = composeEndpointUrl(adapterScheme, adapterHost, adapterPort, adapterPath);
  }

  function populateAdapterDefaults(type) {
    const meta = adapterTypeMeta(type);
    if (!adapterHost) {
      adapterScheme = meta.defaultScheme;
      adapterHost   = meta.defaultHost;
      adapterPort   = meta.defaultPort;
      adapterPath   = meta.defaultPath;
      syncAdapterUrlFromParts();
    }
  }

  // ── Enrichers ────────────────────────────────────────────────────────────────
  let enrichers       = $state([]);
  let enricherAudit   = $state([]);
  let enricherLoading = $state(true);
  let showEnricherForm = $state(false);
  let savingEnricher   = $state(false);
  let testingEnricher  = $state('');
  let enricherTestResult = $state(null);
  let runningEnricher  = $state('');

  const ENRICHER_TYPES = [
    { value: 'netbox',      label: 'NetBox (IPAM/DCIM)',
      hint: 'devices, VLANs, prefixes, racks, HostEndpoints → graph',
      cssVar: '--vendor-netbox' },
    { value: 'servicenow',  label: 'ServiceNow CMDB',
      hint: 'CI records, relationships → graph enrichment properties',
      cssVar: '--vendor-servicenow' },
    { value: 'stub',        label: 'Stub (testing only)',
      hint: 'no-op enricher for CI pipelines',
      cssVar: '--vendor-stub' },
  ];

  function emptyEnricher() {
    return {
      name: '', enricher_type: 'netbox',
      enabled: true, base_url: '', credential_alias: '',
      poll_interval_secs: 3600, environment_scope: [], extra: {},
    };
  }

  let enricherForm     = $state(emptyEnricher());
  let enricherEnvInput = $state('');

  // ── Notifications ─────────────────────────────────────────────────────────────
  let notice = $state(null); // { kind: 'ok'|'err', text }

  function notify(kind, text) {
    notice = { kind, text };
    setTimeout(() => notice = null, 5000);
  }

  // ── Load ─────────────────────────────────────────────────────────────────────
  async function loadAdapters() {
    adapterLoading = true;
    try {
      const [aRes, auRes] = await Promise.all([
        fetch('/api/adapters'),
        fetch('/api/adapters/audit'),
      ]);
      if (aRes.ok)  adapters     = (await aRes.json()).adapters ?? [];
      if (auRes.ok) adapterAudit = (await auRes.json()).entries ?? [];
    } catch (e) {
      notify('err', 'Failed to load adapters: ' + e.message);
    } finally {
      adapterLoading = false;
    }
  }

  async function loadEnrichers() {
    enricherLoading = true;
    try {
      const [eRes, auRes] = await Promise.all([
        fetch('/api/enrichment'),
        fetch('/api/enrichment/audit'),
      ]);
      if (eRes.ok)  enrichers     = (await eRes.json()).enrichers ?? [];
      if (auRes.ok) enricherAudit = (await auRes.json()).entries ?? [];
    } catch (e) {
      notify('err', 'Failed to load enrichers: ' + e.message);
    } finally {
      enricherLoading = false;
    }
  }

  onMount(() => {
    loadAdapters();
    loadEnrichers();
  });

  // ── Output adapter CRUD ───────────────────────────────────────────────────────
  function openNewAdapter() {
    adapterForm = emptyAdapter();
    editingAdapterName = '';
    adapterEnvInput = '';
    adapterTestResult = null;
    const meta = adapterTypeMeta('prometheus_remote_write');
    adapterScheme = meta.defaultScheme;
    adapterHost   = '';
    adapterPort   = meta.defaultPort;
    adapterPath   = meta.defaultPath;
    showAdapterForm = true;
  }

  function openEditAdapter(a) {
    adapterForm = structuredClone(a.config);
    editingAdapterName = a.config.name;
    adapterEnvInput = (a.config.environment_scope ?? []).join(', ');
    adapterTestResult = null;
    const parsed = parseEndpointUrl(a.config.endpoint_url);
    adapterScheme = parsed.scheme;
    adapterHost   = parsed.host;
    adapterPort   = parsed.port;
    adapterPath   = parsed.path;
    showAdapterForm = true;
  }

  function cancelAdapterForm() {
    showAdapterForm = false;
    adapterTestResult = null;
  }

  async function saveAdapter() {
    savingAdapter = true;
    syncAdapterUrlFromParts();
    adapterForm.environment_scope = adapterEnvInput.split(',').map(s => s.trim()).filter(Boolean);
    try {
      const res = await fetch('/api/adapters', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ config: adapterForm }),
      });
      const data = await res.json();
      if (data.success) {
        showAdapterForm = false;
        notify('ok', `Adapter "${adapterForm.name}" saved.`);
        await loadAdapters();
      } else {
        notify('err', 'Save failed: ' + (data.error ?? 'unknown error'));
      }
    } catch (e) {
      notify('err', 'Save error: ' + e.message);
    } finally {
      savingAdapter = false;
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
      if (data.success) { notify('ok', `Adapter "${name}" removed.`); await loadAdapters(); }
      else notify('err', 'Remove failed: ' + (data.error ?? 'unknown'));
    } catch (e) {
      notify('err', 'Remove error: ' + e.message);
    }
  }

  async function testAdapter(name) {
    testingAdapter = name;
    adapterTestResult = null;
    try {
      const res = await fetch('/api/adapters/test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const data = await res.json();
      adapterTestResult = { name, success: data.success, message: data.message };
    } catch (e) {
      adapterTestResult = { name, success: false, message: e.message };
    } finally {
      testingAdapter = '';
    }
  }

  async function testAdapterInForm() {
    if (!adapterForm.name || !adapterForm.endpoint_url) return;
    await testAdapter(adapterForm.name);
  }

  // ── Enricher CRUD ─────────────────────────────────────────────────────────────
  function openNewEnricher() {
    enricherForm = emptyEnricher();
    enricherEnvInput = '';
    enricherTestResult = null;
    showEnricherForm = true;
  }

  function openEditEnricher(e) {
    enricherForm = { ...e.config };
    enricherEnvInput = (e.config.environment_scope ?? []).join(', ');
    enricherTestResult = null;
    showEnricherForm = true;
  }

  function cancelEnricherForm() {
    showEnricherForm = false;
    enricherTestResult = null;
  }

  async function saveEnricher() {
    if (!enricherForm.name.trim() || !enricherForm.base_url.trim()) {
      notify('err', 'Name and Base URL are required.');
      return;
    }
    savingEnricher = true;
    enricherForm.environment_scope = enricherEnvInput.split(',').map(s => s.trim()).filter(Boolean);
    enricherForm.poll_interval_secs = Number(enricherForm.poll_interval_secs);
    try {
      const res = await fetch('/api/enrichment', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ config: enricherForm }),
      });
      const data = await res.json();
      if (data.success) {
        showEnricherForm = false;
        notify('ok', `Enricher "${enricherForm.name}" saved.`);
        await loadEnrichers();
      } else {
        notify('err', 'Save failed: ' + (data.error ?? 'unknown'));
      }
    } catch (e) {
      notify('err', 'Save error: ' + e.message);
    } finally {
      savingEnricher = false;
    }
  }

  async function removeEnricher(name) {
    if (!confirm(`Remove enricher "${name}"?`)) return;
    try {
      const res = await fetch('/api/enrichment/remove', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const data = await res.json();
      if (data.success) { notify('ok', `Enricher "${name}" removed.`); await loadEnrichers(); }
      else notify('err', 'Remove failed: ' + (data.error ?? 'unknown'));
    } catch (e) {
      notify('err', 'Remove error: ' + e.message);
    }
  }

  async function testEnricher(name) {
    testingEnricher = name;
    enricherTestResult = null;
    try {
      const res = await fetch('/api/enrichment/test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const data = await res.json();
      enricherTestResult = { name, success: data.success, message: data.message };
    } catch (e) {
      enricherTestResult = { name, success: false, message: e.message };
    } finally {
      testingEnricher = '';
    }
  }

  async function runEnricher(name) {
    runningEnricher = name;
    try {
      const res = await fetch('/api/enrichment/run', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const data = await res.json();
      if (data.success) { notify('ok', data.message); setTimeout(loadEnrichers, 1500); }
      else notify('err', data.error ?? 'run failed');
    } catch (e) {
      notify('err', e.message);
    } finally {
      runningEnricher = '';
    }
  }

  // ── Extra-field helpers (per-type) ────────────────────────────────────────────
  function getExtra(obj, key, def = '') {
    return obj.extra?.[key] ?? def;
  }

  function setExtra(obj, key, val) {
    obj.extra = { ...(obj.extra ?? {}), [key]: val };
  }

  // ── Format helpers ────────────────────────────────────────────────────────────
  function fmtNs(ns) {
    if (!ns) return '—';
    return new Date(Math.floor(ns / 1_000_000)).toLocaleString();
  }

  function fmtBytes(b) {
    if (!b) return '0 B';
    if (b < 1024) return b + ' B';
    if (b < 1048576) return (b / 1024).toFixed(1) + ' KB';
    return (b / 1048576).toFixed(1) + ' MB';
  }

  function fmtDuration(ms) {
    if (ms == null) return '—';
    if (ms < 1000) return `${ms} ms`;
    return `${(ms / 1000).toFixed(1)} s`;
  }

  function adapterTypeLabel(t) {
    return ADAPTER_TYPES.find(a => a.value === t)?.label ?? t;
  }

  function adapterCssVar(t) {
    return ADAPTER_TYPES.find(a => a.value === t)?.cssVar ?? '--vendor-stub';
  }

  function enricherTypeLabel(t) {
    return ENRICHER_TYPES.find(e => e.value === t)?.label ?? t;
  }

  function enricherCssVar(t) {
    return ENRICHER_TYPES.find(e => e.value === t)?.cssVar ?? '--vendor-stub';
  }
</script>

<!-- ═══════════════════════════════════════════════════════════════════════════ -->
<div class="integrations-workspace">

  <div class="page-header">
    <div>
      <h1>Integrations</h1>
      <p class="subtitle">Configure external enrichment sources and output destinations. All credentials are resolved from the vault.</p>
    </div>
  </div>

  <!-- Notification banner -->
  {#if notice}
    <div class="notice notice-{notice.kind}">
      {notice.kind === 'ok' ? '✓' : '✗'} {notice.text}
      <button class="notice-dismiss" onclick={() => notice = null}>×</button>
    </div>
  {/if}

  <!-- Tab bar -->
  <div class="tab-bar">
    <button class="tab-btn" class:active={tab === 'enrichment'} onclick={() => tab = 'enrichment'}>
      <span class="tab-icon">⟳</span> Enrichment Sources
      <span class="tab-count">{enrichers.length}</span>
    </button>
    <button class="tab-btn" class:active={tab === 'output'} onclick={() => tab = 'output'}>
      <span class="tab-icon">⇥</span> Output Adapters
      <span class="tab-count">{adapters.length}</span>
    </button>
  </div>

  <!-- ─────────────────────────────────────────────────────────────────────────
       ENRICHMENT TAB
  ───────────────────────────────────────────────────────────────────────────── -->
  {#if tab === 'enrichment'}
    <div class="section-actions">
      <button class="btn-secondary" onclick={loadEnrichers}>↺ Refresh</button>
      <button class="btn-primary" onclick={openNewEnricher}>+ Add enricher</button>
    </div>

    <!-- Architecture callout -->
    <div class="arch-callout">
      <strong>How enrichment works:</strong>
      Enrichers pull context (device info, VLANs, prefixes, CIs) from external CMDBs/IPAMs
      and write it into the bonsai graph as <code>netbox_*</code> / <code>snow_*</code> properties plus
      first-class VLAN, Prefix, Rack, Location, and HostEndpoint nodes.
      Runs on a configurable schedule or on-demand.
    </div>

    <!-- Enricher form -->
    {#if showEnricherForm}
      <div class="integration-form card">
        <div class="form-title">
          <span class="form-type-dot" style="background: var({enricherCssVar(enricherForm.enricher_type)})"></span>
          <h2>{enricherForm.name ? `Edit: ${enricherForm.name}` : 'New enricher'}</h2>
        </div>

        <div class="form-grid">
          <label>
            Name <span class="req">*</span>
            <input type="text" bind:value={enricherForm.name} placeholder="netbox-prod" />
          </label>

          <label>
            Type <span class="req">*</span>
            <select bind:value={enricherForm.enricher_type}>
              {#each ENRICHER_TYPES as t}
                <option value={t.value}>{t.label}</option>
              {/each}
            </select>
            <span class="hint">{ENRICHER_TYPES.find(t => t.value === enricherForm.enricher_type)?.hint ?? ''}</span>
          </label>

          <label class="span-2">
            Base URL <span class="req">*</span>
            <input type="url" bind:value={enricherForm.base_url} placeholder="http://netbox:8000" />
          </label>

          <label>
            Credential alias
            <input type="text" bind:value={enricherForm.credential_alias} placeholder="netbox-token" />
            <span class="hint">Add the credential in Credentials first.</span>
          </label>

          <label>
            Poll interval (seconds)
            <input type="number" bind:value={enricherForm.poll_interval_secs} min="0" placeholder="3600" />
            <span class="hint">0 = manual only</span>
          </label>

          <label class="span-2">
            Environment scope
            <input type="text" bind:value={enricherEnvInput} placeholder="data_center, campus  (empty = all)" />
          </label>

          <label class="checkbox-label">
            <input type="checkbox" bind:checked={enricherForm.enabled} />
            Enabled
          </label>
        </div>

        <!-- Type-specific extra fields -->
        {#if enricherForm.enricher_type === 'netbox'}
          <div class="extras-section">
            <h3 class="extras-title">NetBox advanced options</h3>
            <div class="form-grid">
              <label>
                Transport
                <select
                  value={getExtra(enricherForm, 'transport', 'rest')}
                  onchange={e => setExtra(enricherForm, 'transport', e.target.value)}
                >
                  <option value="rest">REST (direct)</option>
                  <option value="mcp">MCP proxy</option>
                </select>
                <span class="hint">Use MCP if NetBox is behind a tool-call gateway.</span>
              </label>

              {#if getExtra(enricherForm, 'transport') === 'mcp'}
                <label>
                  MCP server URL
                  <input type="url"
                    value={getExtra(enricherForm, 'mcp_server_url', 'http://localhost:8090')}
                    oninput={e => setExtra(enricherForm, 'mcp_server_url', e.target.value)}
                    placeholder="http://mcp-gateway:8090"
                  />
                </label>
              {/if}

              <label class="span-2">
                Endpoint roles (HostEndpoint classification)
                <input type="text"
                  value={getExtra(enricherForm, 'endpoint_roles', 'server,ap,phone,cpe,printer,workstation')}
                  oninput={e => setExtra(enricherForm, 'endpoint_roles', e.target.value)}
                  placeholder="server,ap,phone,cpe,printer,workstation"
                />
                <span class="hint">Comma-separated NetBox role slugs that map to HostEndpoint nodes (not network Devices).</span>
              </label>

              <label>
                Max concurrent requests
                <input type="number"
                  value={getExtra(enricherForm, 'max_concurrent_requests', 2)}
                  oninput={e => setExtra(enricherForm, 'max_concurrent_requests', Number(e.target.value))}
                  min="1" max="10"
                />
                <span class="hint">Limits in-flight REST calls to NetBox. Default: 2.</span>
              </label>
            </div>
          </div>
        {/if}

        {#if enricherForm.enricher_type === 'servicenow'}
          <div class="extras-section">
            <h3 class="extras-title">ServiceNow CMDB options</h3>
            <div class="form-grid">
              <label class="span-2">
                CI table
                <input type="text"
                  value={getExtra(enricherForm, 'ci_table', 'cmdb_ci_netgear')}
                  oninput={e => setExtra(enricherForm, 'ci_table', e.target.value)}
                  placeholder="cmdb_ci_netgear"
                />
                <span class="hint">ServiceNow CMDB table to read CI records from.</span>
              </label>
            </div>
          </div>
        {/if}

        <!-- Test result inside form -->
        {#if enricherTestResult && enricherTestResult.name === enricherForm.name}
          <div class="inline-test-result" class:test-ok={enricherTestResult.success} class:test-fail={!enricherTestResult.success}>
            {enricherTestResult.success ? '✓ Connected' : '✗ Failed'}: {enricherTestResult.message}
          </div>
        {/if}

        <div class="form-actions">
          <button class="btn-ghost" onclick={cancelEnricherForm}>Cancel</button>
          <button class="btn-secondary" onclick={() => testEnricher(enricherForm.name)}
                  disabled={!enricherForm.name || testingEnricher === enricherForm.name}>
            {testingEnricher === enricherForm.name ? 'Testing…' : 'Test connection'}
          </button>
          <button class="btn-primary" onclick={saveEnricher}
                  disabled={savingEnricher || !enricherForm.name || !enricherForm.base_url}>
            {savingEnricher ? 'Saving…' : 'Save enricher'}
          </button>
        </div>
      </div>
    {/if}

    <!-- Test result banner (outside form) -->
    {#if enricherTestResult && !showEnricherForm}
      <div class="test-banner" class:test-ok={enricherTestResult.success} class:test-fail={!enricherTestResult.success}>
        <strong>{enricherTestResult.name}</strong>: {enricherTestResult.success ? '✓ ' : '✗ '}{enricherTestResult.message}
        <button class="banner-dismiss" onclick={() => enricherTestResult = null}>×</button>
      </div>
    {/if}

    <!-- Enricher cards -->
    {#if enricherLoading}
      <div class="loading">Loading enrichers…</div>
    {:else if enrichers.length === 0}
      <div class="empty-state">
        <p>No enrichers configured.</p>
        <p>Add a <strong>NetBox</strong> enricher to start populating device rack, site, VLAN,
           prefix, and host-endpoint context into the graph.</p>
      </div>
    {:else}
      {#each enrichers as e (e.config.name)}
        {@const cfg = e.config}
        {@const st = e.state}
        <div class="integration-card card" class:disabled-card={!cfg.enabled}>
          <div class="card-header">
            <div class="card-title">
              <span class="type-pip" style="background: var({enricherCssVar(cfg.enricher_type)})"></span>
              <strong>{cfg.name}</strong>
              <span class="type-badge" style="border-color: var({enricherCssVar(cfg.enricher_type)}); color: var({enricherCssVar(cfg.enricher_type)})">
                {enricherTypeLabel(cfg.enricher_type)}
              </span>
              {#if !cfg.enabled}
                <span class="disabled-badge">disabled</span>
              {/if}
            </div>
            <div class="card-actions">
              <button class="btn-sm" onclick={() => testEnricher(cfg.name)}
                      disabled={testingEnricher === cfg.name}>
                {testingEnricher === cfg.name ? 'Testing…' : 'Test'}
              </button>
              <button class="btn-sm" onclick={() => runEnricher(cfg.name)}
                      disabled={st.is_running || runningEnricher === cfg.name}>
                {st.is_running || runningEnricher === cfg.name ? 'Running…' : 'Run now'}
              </button>
              <button class="btn-sm" onclick={() => openEditEnricher(e)}>Edit</button>
              <button class="btn-sm btn-danger" onclick={() => removeEnricher(cfg.name)}>Remove</button>
            </div>
          </div>

          <div class="card-meta">
            <span>URL: <code>{cfg.base_url}</code></span>
            <span>Cred: <code>{cfg.credential_alias || '—'}</code></span>
            <span>Poll: {cfg.poll_interval_secs ? cfg.poll_interval_secs + 's' : 'manual'}</span>
            {#if cfg.extra?.transport === 'mcp'}
              <span class="extra-badge">MCP transport</span>
            {/if}
            {#if cfg.extra?.endpoint_roles}
              <span class="extra-badge">roles: {cfg.extra.endpoint_roles}</span>
            {/if}
            {#if cfg.environment_scope?.length}
              <span>Scope: {cfg.environment_scope.join(', ')}</span>
            {:else}
              <span class="dim">All environments</span>
            {/if}
          </div>

          <div class="card-run-state">
            {#if st.last_run_at_ns}
              <span class:state-error={!!st.last_run_error}>
                Last run: {fmtNs(st.last_run_at_ns)}
                {#if st.last_run_duration_ms != null}· {fmtDuration(st.last_run_duration_ms)}{/if}
                {#if st.last_run_nodes_touched != null}· {st.last_run_nodes_touched} nodes touched{/if}
              </span>
              {#if st.last_run_error}
                <span class="run-error">Error: {st.last_run_error}</span>
              {/if}
              {#if st.last_run_warnings?.length}
                <details class="run-warnings">
                  <summary>{st.last_run_warnings.length} warning(s)</summary>
                  {#each st.last_run_warnings as w}<p class="warning-line">{w}</p>{/each}
                </details>
              {/if}
            {:else}
              <span class="dim">Never run.</span>
            {/if}
          </div>
        </div>
      {/each}
    {/if}

    <!-- Enrichment audit log -->
    <section class="audit-section">
      <h2>Enrichment run history</h2>
      {#if enricherAudit.length === 0}
        <p class="dim">No enrichment runs recorded yet.</p>
      {:else}
        <table class="audit-table">
          <thead>
            <tr><th>Time</th><th>Enricher</th><th>Outcome</th><th>Nodes</th><th>Error</th></tr>
          </thead>
          <tbody>
            {#each enricherAudit.slice(0, 50) as entry}
              <tr class:row-error={entry.outcome !== 'success'}>
                <td>{fmtNs(entry.timestamp_ns)}</td>
                <td><code>{entry.enricher}</code></td>
                <td>
                  <span class="outcome-badge" class:outcome-ok={entry.outcome === 'success'} class:outcome-err={entry.outcome !== 'success'}>
                    {entry.outcome}
                  </span>
                </td>
                <td>{entry.nodes_touched ?? '—'}</td>
                <td class="error-cell">{entry.error ?? ''}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>

  <!-- ─────────────────────────────────────────────────────────────────────────
       OUTPUT ADAPTERS TAB
  ───────────────────────────────────────────────────────────────────────────── -->
  {:else}
    <div class="section-actions">
      <button class="btn-secondary" onclick={loadAdapters}>↺ Refresh</button>
      <button class="btn-primary" onclick={openNewAdapter}>+ Add adapter</button>
    </div>

    <!-- Architecture callout -->
    <div class="arch-callout">
      <strong>How output adapters work:</strong>
      Adapters subscribe to the internal event bus and push data to external systems.
      <strong>Prometheus Remote Write</strong> pushes raw telemetry counters (collector-side, from gNMI).
      <strong>Splunk HEC, Elasticsearch,</strong> and <strong>ServiceNow EM</strong> push detection events
      (core-side, from the graph) on a configurable flush interval with cursor persistence across restarts.
    </div>

    <!-- Adapter form -->
    {#if showAdapterForm}
      <div class="integration-form card">
        <div class="form-title">
          <span class="form-type-dot" style="background: var({adapterCssVar(adapterForm.adapter_type)})"></span>
          <h2>{editingAdapterName ? `Edit: ${editingAdapterName}` : 'New output adapter'}</h2>
        </div>

        <div class="form-grid">
          <label>
            Name <span class="req">*</span>
            <input type="text" bind:value={adapterForm.name} placeholder="splunk-prod"
                   disabled={!!editingAdapterName} />
          </label>

          <label>
            Type <span class="req">*</span>
            <select bind:value={adapterForm.adapter_type}
                    onchange={(e) => populateAdapterDefaults(e.target.value)}>
              {#each ADAPTER_TYPES as t}
                <option value={t.value}>{t.label}</option>
              {/each}
            </select>
            <span class="hint">{ADAPTER_TYPES.find(t => t.value === adapterForm.adapter_type)?.hint ?? ''}</span>
          </label>

          <!-- ── Endpoint: structured scheme + host + port + path ── -->
          <div class="span-2 endpoint-row">
            <span class="field-label">Endpoint <span class="req">*</span></span>
            <div class="endpoint-fields">
              <label class="endpoint-scheme">
                <span class="field-sublabel">Scheme</span>
                <select bind:value={adapterScheme} onchange={syncAdapterUrlFromParts}>
                  <option value="http">http</option>
                  <option value="https">https</option>
                </select>
              </label>
              <label class="endpoint-host">
                <span class="field-sublabel">Host / IP</span>
                <input type="text" bind:value={adapterHost}
                       oninput={syncAdapterUrlFromParts}
                       placeholder={adapterTypeMeta(adapterForm.adapter_type).defaultHost} />
              </label>
              <label class="endpoint-port">
                <span class="field-sublabel">Port</span>
                <input type="number" bind:value={adapterPort}
                       oninput={syncAdapterUrlFromParts}
                       min="1" max="65535"
                       placeholder={adapterTypeMeta(adapterForm.adapter_type).defaultPort} />
              </label>
              <label class="endpoint-path">
                <span class="field-sublabel">Path (optional)</span>
                <input type="text" bind:value={adapterPath}
                       oninput={syncAdapterUrlFromParts}
                       placeholder="/api/v1/write" />
              </label>
            </div>
            <code class="endpoint-preview">{adapterForm.endpoint_url || 'http://host:port/path'}</code>
          </div>

          <label>
            Credential alias
            <input type="text" bind:value={adapterForm.credential_alias}
                   placeholder="(leave empty for no auth)" />
          </label>

          <label>
            Flush interval (s)
            <input type="number" bind:value={adapterForm.flush_interval_secs} min="5" max="3600" />
          </label>

          <label class="span-2">
            Environment scope
            <input type="text" bind:value={adapterEnvInput}
                   placeholder="data_center, service_provider  (empty = all)" />
          </label>

          <label class="checkbox-label">
            <input type="checkbox" bind:checked={adapterForm.enabled} />
            Enabled
          </label>
        </div>

        <!-- Type-specific extra fields -->
        {#if adapterForm.adapter_type === 'prometheus_remote_write'}
          <div class="extras-section">
            <h3 class="extras-title">Prometheus options</h3>
            <div class="form-grid">
              <label>
                Job label
                <input type="text"
                  value={getExtra(adapterForm, 'job', 'bonsai')}
                  oninput={e => setExtra(adapterForm, 'job', e.target.value)}
                  placeholder="bonsai"
                />
                <span class="hint">Prometheus <code>job</code> label on all metrics. Default: <code>bonsai</code>.</span>
              </label>
            </div>
          </div>

        {:else if adapterForm.adapter_type === 'splunk_hec'}
          <div class="extras-section">
            <h3 class="extras-title">Splunk HEC options</h3>
            <div class="form-grid">
              <label>
                Sourcetype
                <input type="text"
                  value={getExtra(adapterForm, 'sourcetype', 'bonsai:detection')}
                  oninput={e => setExtra(adapterForm, 'sourcetype', e.target.value)}
                  placeholder="bonsai:detection"
                />
                <span class="hint">Splunk sourcetype. Default: <code>bonsai:detection</code>.</span>
              </label>

              <label>
                Index
                <input type="text"
                  value={getExtra(adapterForm, 'index', '')}
                  oninput={e => setExtra(adapterForm, 'index', e.target.value)}
                  placeholder="(token default)"
                />
                <span class="hint">Leave empty to use the HEC token's default index.</span>
              </label>

              <label>
                Dedup window (seconds)
                <input type="number"
                  value={getExtra(adapterForm, 'dedup_window_secs', 300)}
                  oninput={e => setExtra(adapterForm, 'dedup_window_secs', Number(e.target.value))}
                  min="0"
                />
                <span class="hint">Suppress re-push of (device, rule) within this window. Default: 300.</span>
              </label>

              <label class="checkbox-label">
                <input type="checkbox"
                  checked={getExtra(adapterForm, 'insecure_tls', false)}
                  onchange={e => setExtra(adapterForm, 'insecure_tls', e.target.checked)}
                />
                Skip TLS verification (lab use only)
              </label>
            </div>
          </div>

        {:else if adapterForm.adapter_type === 'elastic'}
          <div class="extras-section">
            <h3 class="extras-title">Elasticsearch options</h3>
            <div class="form-grid">
              <label>
                Index
                <input type="text"
                  value={getExtra(adapterForm, 'index', 'bonsai-detections')}
                  oninput={e => setExtra(adapterForm, 'index', e.target.value)}
                  placeholder="bonsai-detections"
                />
                <span class="hint">Target index. Default: <code>bonsai-detections</code>.</span>
              </label>

              <label>
                Auth type
                <select
                  value={getExtra(adapterForm, 'auth_type', 'basic')}
                  onchange={e => setExtra(adapterForm, 'auth_type', e.target.value)}
                >
                  <option value="basic">Basic auth (username/password)</option>
                  <option value="api_key">API Key (base64 id:key in vault password)</option>
                </select>
                <span class="hint">Set credential alias above with the matching vault entry.</span>
              </label>

              <label>
                Dedup window (seconds)
                <input type="number"
                  value={getExtra(adapterForm, 'dedup_window_secs', 300)}
                  oninput={e => setExtra(adapterForm, 'dedup_window_secs', Number(e.target.value))}
                  min="0"
                />
                <span class="hint">Suppress re-push of (device, rule) within this window.</span>
              </label>
            </div>
          </div>

        {:else if adapterForm.adapter_type === 'servicenow_em'}
          <div class="extras-section">
            <h3 class="extras-title">ServiceNow Event Management options</h3>
            <div class="form-grid">
              <label>
                Minimum severity
                <select
                  value={getExtra(adapterForm, 'min_severity', 'warning')}
                  onchange={e => setExtra(adapterForm, 'min_severity', e.target.value)}
                >
                  <option value="critical">Critical only</option>
                  <option value="high">High and above</option>
                  <option value="warning">Warning and above</option>
                  <option value="info">All (info and above)</option>
                </select>
                <span class="hint">Only push events at or above this severity to ServiceNow EM.</span>
              </label>

              <label>
                Min age before push (seconds)
                <input type="number"
                  value={getExtra(adapterForm, 'min_age_secs', 60)}
                  oninput={e => setExtra(adapterForm, 'min_age_secs', Number(e.target.value))}
                  min="0"
                />
                <span class="hint">Detection must be at least this old before pushing. Avoids noisy flap events. Default: 60.</span>
              </label>

              <label>
                Dedup window (seconds)
                <input type="number"
                  value={getExtra(adapterForm, 'dedup_window_secs', 300)}
                  oninput={e => setExtra(adapterForm, 'dedup_window_secs', Number(e.target.value))}
                  min="0"
                />
                <span class="hint">Suppress re-push of (device, rule) within this window. Default: 300.</span>
              </label>

              <label>
                Severity mapping reference
                <div class="severity-map-table">
                  <div class="smt-row"><span>critical</span><span>→</span><span>1 Critical</span></div>
                  <div class="smt-row"><span>high</span><span>→</span><span>2 Major</span></div>
                  <div class="smt-row"><span>warning</span><span>→</span><span>3 Minor</span></div>
                  <div class="smt-row"><span>info</span><span>→</span><span>5 Informational</span></div>
                </div>
              </label>
            </div>
          </div>
        {/if}

        <!-- Inline test result -->
        {#if adapterTestResult && adapterTestResult.name === adapterForm.name}
          <div class="inline-test-result" class:test-ok={adapterTestResult.success} class:test-fail={!adapterTestResult.success}>
            {adapterTestResult.success ? '✓ Connected' : '✗ Failed'}: {adapterTestResult.message}
          </div>
        {/if}

        <div class="form-actions">
          <button class="btn-ghost" onclick={cancelAdapterForm}>Cancel</button>
          <button class="btn-secondary"
                  onclick={testAdapterInForm}
                  disabled={!adapterForm.name || !adapterForm.endpoint_url || testingAdapter === adapterForm.name}>
            {testingAdapter === adapterForm.name ? 'Testing…' : 'Test connection'}
          </button>
          <button class="btn-primary" onclick={saveAdapter}
                  disabled={savingAdapter || !adapterForm.name || !adapterForm.endpoint_url}>
            {savingAdapter ? 'Saving…' : 'Save adapter'}
          </button>
        </div>
      </div>
    {/if}

    <!-- Test result banner (outside form) -->
    {#if adapterTestResult && !showAdapterForm}
      <div class="test-banner" class:test-ok={adapterTestResult.success} class:test-fail={!adapterTestResult.success}>
        <strong>{adapterTestResult.name}</strong>: {adapterTestResult.success ? '✓ ' : '✗ '}{adapterTestResult.message}
        <button class="banner-dismiss" onclick={() => adapterTestResult = null}>×</button>
      </div>
    {/if}

    <!-- Adapter cards -->
    {#if adapterLoading}
      <div class="loading">Loading adapters…</div>
    {:else if adapters.length === 0}
      <div class="empty-state">
        <p>No output adapters configured.</p>
        <p>Add a <strong>Prometheus Remote Write</strong> adapter to export interface telemetry
           to Prometheus/Grafana, or a <strong>Splunk HEC</strong> / <strong>Elasticsearch</strong>
           adapter to forward detection events to your SIEM.</p>
      </div>
    {:else}
      {#each adapters as a (a.config.name)}
        {@const cfg = a.config}
        {@const st = a.state}
        <div class="integration-card card" class:disabled-card={!cfg.enabled}>
          <div class="card-header">
            <div class="card-title">
              <span class="type-pip" style="background: var({adapterCssVar(cfg.adapter_type)})"></span>
              <strong>{cfg.name}</strong>
              <span class="type-badge" style="border-color: var({adapterCssVar(cfg.adapter_type)}); color: var({adapterCssVar(cfg.adapter_type)})">
                {adapterTypeLabel(cfg.adapter_type)}
              </span>
              {#if !cfg.enabled}
                <span class="disabled-badge">disabled</span>
              {/if}
              {#if st.last_push_error}
                <span class="error-indicator" title={st.last_push_error}>⚠ error</span>
              {:else if st.last_push_at_ns}
                <span class="ok-indicator">✓ healthy</span>
              {/if}
            </div>
            <div class="card-actions">
              <button class="btn-sm" onclick={() => testAdapter(cfg.name)}
                      disabled={testingAdapter === cfg.name}>
                {testingAdapter === cfg.name ? 'Testing…' : 'Test'}
              </button>
              <button class="btn-sm" onclick={() => openEditAdapter(a)}>Edit</button>
              <button class="btn-sm btn-danger" onclick={() => removeAdapter(cfg.name)}>Remove</button>
            </div>
          </div>

          <div class="card-meta">
            <span>URL: <code>{cfg.endpoint_url}</code></span>
            {#if cfg.credential_alias}
              <span>Cred: <code>{cfg.credential_alias}</code></span>
            {:else}
              <span class="dim">No auth</span>
            {/if}
            <span>Flush: {cfg.flush_interval_secs}s</span>
            <!-- Per-type extra hints -->
            {#if cfg.adapter_type === 'splunk_hec' && cfg.extra?.sourcetype}
              <span class="extra-badge">sourcetype: {cfg.extra.sourcetype}</span>
            {/if}
            {#if (cfg.adapter_type === 'elastic' || cfg.adapter_type === 'splunk_hec') && cfg.extra?.index}
              <span class="extra-badge">index: {cfg.extra.index}</span>
            {/if}
            {#if cfg.adapter_type === 'elastic' && cfg.extra?.auth_type === 'api_key'}
              <span class="extra-badge">API key auth</span>
            {/if}
            {#if cfg.adapter_type === 'servicenow_em' && cfg.extra?.min_severity}
              <span class="extra-badge">min: {cfg.extra.min_severity}</span>
            {/if}
            {#if cfg.environment_scope?.length}
              <span>Scope: {cfg.environment_scope.join(', ')}</span>
            {:else}
              <span class="dim">All environments</span>
            {/if}
          </div>

          <div class="card-run-state">
            {#if st.last_push_at_ns}
              <span class:state-error={!!st.last_push_error}>
                Last push: {fmtNs(st.last_push_at_ns)}
                · {st.last_push_events ?? 0} events
                · {fmtBytes(st.last_push_bytes ?? 0)}
                {#if st.last_push_duration_ms}· {st.last_push_duration_ms}ms{/if}
              </span>
              {#if st.last_push_error}
                <span class="run-error">Error: {st.last_push_error}</span>
              {/if}
            {:else}
              <span class="dim">No push recorded yet — adapter starts on next server boot.</span>
            {/if}
            {#if st.total_events_pushed > 0}
              <span class="totals">
                Total: {st.total_events_pushed.toLocaleString()} events · {fmtBytes(st.total_bytes_sent ?? 0)}
              </span>
            {/if}
          </div>
        </div>
      {/each}
    {/if}

    <!-- Adapter audit log -->
    <section class="audit-section">
      <h2>Push audit log</h2>
      {#if adapterAudit.length === 0}
        <p class="dim">No adapter push events recorded yet.</p>
      {:else}
        <table class="audit-table">
          <thead>
            <tr><th>Time</th><th>Adapter</th><th>Outcome</th><th>Events</th><th>Bytes</th><th>Error</th></tr>
          </thead>
          <tbody>
            {#each adapterAudit.slice(0, 50) as entry}
              <tr class:row-error={entry.outcome === 'error'}>
                <td>{fmtNs(entry.timestamp_ns)}</td>
                <td><code>{entry.adapter}</code></td>
                <td>
                  <span class="outcome-badge" class:outcome-ok={entry.outcome === 'success'} class:outcome-err={entry.outcome === 'error'}>
                    {entry.outcome}
                  </span>
                </td>
                <td>{entry.events_pushed ?? '—'}</td>
                <td>{entry.bytes_sent ? fmtBytes(entry.bytes_sent) : '—'}</td>
                <td class="error-cell">{entry.error ?? ''}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  {/if}

</div>

<style>
  /* ── Layout ──────────────────────────────────────────────────────────────── */
  .integrations-workspace {
    padding: 1.5rem;
    max-width: 1140px;
  }

  .page-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    margin-bottom: 1.5rem;
    padding-bottom: 1rem;
    border-bottom: 1px solid var(--border-subtle);
  }
  .page-header h1 {
    margin: 0 0 0.25rem;
    font-size: var(--text-display-3);
    font-weight: 700;
    letter-spacing: var(--tracking-display);
  }
  .subtitle {
    margin: 0;
    font-size: var(--text-small);
    color: var(--text-secondary);
  }

  /* ── Tab bar ─────────────────────────────────────────────────────────────── */
  .tab-bar {
    display: flex;
    gap: 0;
    border-bottom: 1px solid var(--border-subtle);
    margin-bottom: 1.25rem;
  }
  .tab-btn {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.6rem 1.2rem;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    cursor: pointer;
    color: var(--text-secondary);
    font-size: var(--text-small);
    font-weight: 500;
    transition:
      color var(--duration-instant) var(--ease-out),
      border-color var(--duration-instant) var(--ease-out);
  }
  .tab-btn.active {
    color: var(--accent-primary);
    border-bottom-color: var(--accent-primary);
  }
  .tab-btn:hover:not(.active) {
    color: var(--text-primary);
    background: var(--bg-hover);
  }
  .tab-icon { font-size: 1rem; }
  .tab-count {
    font-size: 11px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-pill);
    padding: 0 6px;
    min-width: 20px;
    text-align: center;
    color: var(--text-tertiary);
  }
  .tab-btn.active .tab-count {
    background: var(--accent-subtle);
    border-color: var(--accent-glow);
    color: var(--accent-primary);
  }

  /* ── Section actions ─────────────────────────────────────────────────────── */
  .section-actions {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 1rem;
    justify-content: flex-end;
  }

  /* ── Architecture callout ────────────────────────────────────────────────── */
  .arch-callout {
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-left: 3px solid var(--accent-primary);
    border-radius: var(--radius-md);
    padding: 0.7rem 1rem;
    font-size: var(--text-xs);
    color: var(--text-secondary);
    margin-bottom: 1.25rem;
    line-height: 1.6;
  }
  .arch-callout strong { color: var(--text-primary); }

  /* ── Notification banner ─────────────────────────────────────────────────── */
  .notice {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.55rem 1rem;
    border-radius: var(--radius-md);
    margin-bottom: 1rem;
    font-size: var(--text-small);
  }
  .notice-ok  {
    background: var(--state-healthy-bg);
    border: 1px solid var(--state-healthy-border);
    color: var(--state-healthy);
  }
  .notice-err {
    background: var(--state-failed-bg);
    border: 1px solid var(--state-failed-border);
    color: var(--state-failed);
  }
  .notice-dismiss {
    margin-left: auto;
    background: none;
    border: none;
    cursor: pointer;
    color: inherit;
    font-size: 1.1rem;
    line-height: 1;
  }

  /* ── Cards ───────────────────────────────────────────────────────────────── */
  .card {
    background: var(--bg-surface);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-lg);
    padding: var(--card-pad);
    margin-bottom: 0.75rem;
    box-shadow: var(--shadow-sm);
    transition: border-color var(--duration-instant) var(--ease-out);
  }
  .card:hover { border-color: var(--border-default); }
  .disabled-card { opacity: 0.55; }

  .card-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 0.6rem;
  }
  .card-title {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .type-pip {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
    box-shadow: 0 0 4px currentColor;
  }
  .type-badge {
    font-size: 11px;
    border: 1px solid;
    border-radius: var(--radius-sm);
    padding: 1px 7px;
    font-weight: 500;
    letter-spacing: 0.01em;
  }
  .disabled-badge {
    font-size: 11px;
    background: var(--state-neutral-bg);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    padding: 1px 6px;
    color: var(--state-neutral);
  }
  .error-indicator {
    font-size: 12px;
    color: var(--state-failed);
    font-weight: 500;
  }
  .ok-indicator {
    font-size: 12px;
    color: var(--state-healthy);
    font-weight: 500;
  }

  .card-actions {
    display: flex;
    gap: 0.4rem;
    flex-shrink: 0;
  }
  .card-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    font-size: var(--text-xs);
    color: var(--text-secondary);
    margin-bottom: 0.5rem;
    padding-bottom: 0.5rem;
    border-bottom: 1px solid var(--border-subtle);
  }
  .card-meta code {
    color: var(--text-primary);
    font-size: var(--text-mono-sm);
    background: var(--bg-input);
    padding: 1px 5px;
    border-radius: var(--radius-sm);
  }
  .extra-badge {
    font-size: 11px;
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    padding: 1px 6px;
    color: var(--text-primary);
  }

  .card-run-state {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    font-size: var(--text-xs);
    padding-top: 0.4rem;
  }
  .state-error { color: var(--state-failed); }
  .run-error   { color: var(--state-failed); font-size: 11px; }
  .totals      { color: var(--text-secondary); font-size: 11px; }
  .run-warnings summary {
    font-size: 11px;
    color: var(--state-degraded);
    cursor: pointer;
  }
  .warning-line {
    margin: 0.15rem 0;
    font-size: 11px;
    color: var(--state-degraded);
  }

  /* ── Form ────────────────────────────────────────────────────────────────── */
  .integration-form { margin-bottom: 1rem; }
  .form-title {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 1rem;
    padding-bottom: 0.75rem;
    border-bottom: 1px solid var(--border-subtle);
  }
  .form-title h2 { margin: 0; font-size: var(--text-heading-2); }
  .form-type-dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .form-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.75rem;
    margin-bottom: 0.75rem;
  }
  .form-grid label {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    font-size: var(--text-small);
    color: var(--text-secondary);
    font-weight: 500;
  }
  .form-grid input,
  .form-grid select {
    color: var(--text-primary);
    font-size: var(--text-small);
  }
  .span-2 { grid-column: span 2; }
  .req { color: var(--state-failed); font-weight: 700; }
  .hint {
    font-size: 11px;
    color: var(--text-tertiary);
    font-weight: 400;
  }
  .checkbox-label {
    flex-direction: row !important;
    align-items: center;
    gap: 0.5rem;
    cursor: pointer;
    color: var(--text-primary);
  }
  .checkbox-label input[type="checkbox"] {
    width: auto;
    accent-color: var(--accent-primary);
    cursor: pointer;
  }

  /* ── Endpoint row (structured host+port+path) ────────────────────────────── */
  .endpoint-row {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .field-label {
    font-size: var(--text-small);
    color: var(--text-secondary);
    font-weight: 500;
  }
  .endpoint-fields {
    display: grid;
    grid-template-columns: 90px 1fr 100px 1fr;
    gap: 0.5rem;
    align-items: end;
  }
  .endpoint-fields label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: var(--text-small);
    color: var(--text-secondary);
    font-weight: 500;
  }
  .field-sublabel {
    font-size: 11px;
    color: var(--text-tertiary);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: var(--tracking-caps);
  }
  .endpoint-scheme select,
  .endpoint-port input {
    font-family: var(--font-mono);
    font-size: var(--text-mono-sm);
  }
  .endpoint-preview {
    display: block;
    margin-top: 0.25rem;
    padding: 0.3rem 0.6rem;
    background: var(--bg-input);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-sm);
    font-size: var(--text-mono-sm);
    color: var(--accent-primary);
    word-break: break-all;
    user-select: all;
  }

  /* ── Extras section ──────────────────────────────────────────────────────── */
  .extras-section {
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    padding: 0.85rem;
    margin-bottom: 0.85rem;
  }
  .extras-title {
    margin: 0 0 0.65rem;
    font-size: var(--text-small);
    color: var(--text-secondary);
    font-weight: 600;
    letter-spacing: 0.01em;
  }

  /* ── Severity mapping table ──────────────────────────────────────────────── */
  .severity-map-table {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 4px;
  }
  .smt-row {
    display: flex;
    gap: 0.5rem;
    font-size: 11px;
    font-family: var(--font-mono);
    color: var(--text-secondary);
  }
  .smt-row span:first-child {
    width: 65px;
    color: var(--text-primary);
    font-weight: 500;
  }

  /* ── Inline test result ──────────────────────────────────────────────────── */
  .inline-test-result {
    border-radius: var(--radius-md);
    padding: 0.5rem 0.85rem;
    font-size: var(--text-small);
    margin-bottom: 0.75rem;
    font-weight: 500;
  }
  .test-ok {
    background: var(--state-healthy-bg);
    border: 1px solid var(--state-healthy-border);
    color: var(--state-healthy);
  }
  .test-fail {
    background: var(--state-failed-bg);
    border: 1px solid var(--state-failed-border);
    color: var(--state-failed);
  }

  /* ── Test banner ─────────────────────────────────────────────────────────── */
  .test-banner {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.55rem 1rem;
    border-radius: var(--radius-md);
    margin-bottom: 1rem;
    font-size: var(--text-small);
  }
  .banner-dismiss {
    margin-left: auto;
    background: none;
    border: none;
    cursor: pointer;
    color: inherit;
    font-size: 1.1rem;
    line-height: 1;
  }

  /* ── Form actions ────────────────────────────────────────────────────────── */
  .form-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    padding-top: 0.75rem;
    border-top: 1px solid var(--border-subtle);
  }

  /* ── Button overrides (scoped to this component) ─────────────────────────── */
  .btn-primary {
    background: var(--accent-muted);
    color: var(--text-on-accent);
    border: 1px solid var(--accent-muted);
    border-radius: var(--radius-md);
    padding: 0.38rem 0.9rem;
    cursor: pointer;
    font-size: var(--text-small);
    font-weight: 600;
    white-space: nowrap;
    transition: background var(--duration-instant) var(--ease-out);
  }
  .btn-primary:hover:not(:disabled) { background: var(--accent-hover); }
  .btn-primary:disabled { opacity: 0.4; cursor: not-allowed; }

  .btn-secondary {
    background: var(--bg-elevated);
    color: var(--text-primary);
    border: 1px solid var(--border-default);
    border-radius: var(--radius-md);
    padding: 0.38rem 0.9rem;
    cursor: pointer;
    font-size: var(--text-small);
    font-weight: 500;
    white-space: nowrap;
    transition: background var(--duration-instant) var(--ease-out),
                border-color var(--duration-instant) var(--ease-out);
  }
  .btn-secondary:hover:not(:disabled) {
    background: var(--bg-hover);
    border-color: var(--border-strong);
  }

  .btn-ghost {
    background: none;
    color: var(--text-secondary);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    padding: 0.38rem 0.9rem;
    cursor: pointer;
    font-size: var(--text-small);
    font-weight: 500;
    white-space: nowrap;
  }
  .btn-ghost:hover { color: var(--text-primary); background: var(--bg-hover); }

  .btn-sm {
    background: var(--bg-elevated);
    border: 1px solid var(--border-default);
    border-radius: var(--radius-sm);
    padding: 0.2rem 0.6rem;
    font-size: 12px;
    cursor: pointer;
    color: var(--text-secondary);
    font-weight: 500;
    transition: color var(--duration-instant) var(--ease-out),
                background var(--duration-instant) var(--ease-out);
  }
  .btn-sm:hover:not(:disabled) {
    color: var(--text-primary);
    background: var(--bg-hover);
  }
  .btn-sm:disabled { opacity: 0.4; cursor: not-allowed; }
  .btn-danger {
    color: var(--state-failed);
    border-color: var(--state-failed-border);
    background: var(--state-failed-bg);
  }
  .btn-danger:hover:not(:disabled) {
    background: rgba(248,113,113,0.18);
  }

  /* ── Audit ───────────────────────────────────────────────────────────────── */
  .audit-section { margin-top: 2rem; }
  .audit-section h2 {
    font-size: var(--text-heading-2);
    margin-bottom: 0.65rem;
    font-weight: 600;
    letter-spacing: var(--tracking-display);
  }
  .audit-table {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--text-xs);
  }
  .audit-table th {
    text-align: left;
    padding: 0.4rem 0.6rem;
    border-bottom: 1px solid var(--border-default);
    color: var(--text-tertiary);
    font-weight: 700;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: var(--tracking-caps);
  }
  .audit-table td {
    padding: 0.32rem 0.6rem;
    border-bottom: 1px solid var(--border-subtle);
    vertical-align: top;
  }
  .audit-table tr:hover td { background: var(--bg-hover); }
  .row-error td { color: var(--state-failed); }
  .error-cell {
    font-size: 11px;
    max-width: 280px;
    word-break: break-all;
    color: var(--state-failed);
  }
  .outcome-badge {
    font-size: 11px;
    border-radius: var(--radius-sm);
    padding: 1px 6px;
    font-weight: 500;
  }
  .outcome-ok  {
    background: var(--state-healthy-bg);
    color: var(--state-healthy);
  }
  .outcome-err {
    background: var(--state-failed-bg);
    color: var(--state-failed);
  }

  /* ── Misc ────────────────────────────────────────────────────────────────── */
  .dim { color: var(--text-secondary); }
  .loading {
    padding: 2.5rem;
    text-align: center;
    color: var(--text-secondary);
  }
  .empty-state {
    padding: 2.5rem;
    text-align: center;
    color: var(--text-secondary);
    border: 1px dashed var(--border-default);
    border-radius: var(--radius-lg);
    margin-bottom: 1rem;
    line-height: 1.7;
  }

  @media (max-width: 700px) {
    .endpoint-fields {
      grid-template-columns: 1fr 1fr;
    }
    .endpoint-scheme { grid-column: span 2; }
    .form-grid { grid-template-columns: 1fr; }
    .span-2 { grid-column: span 1; }
  }
</style>
