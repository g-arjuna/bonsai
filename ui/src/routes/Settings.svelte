<script>
  import { onMount } from 'svelte';
  import { toast } from '$lib/toast.svelte.js';

  // ── State ───────────────────────────────────────────────────────────────────
  let loading  = $state(true);
  let saving   = $state(false);
  let settings = $state(null);
  let dirty    = $state(false);
  let liveStatus = $state({});   // name → ReceiverStatusSnapshot
  let statusPollTimer = null;
  let aiCfg = $state(null);
  let aiTesting = $state(false);
  let aiTestResult = $state(null);

  // ── LLM Provider management (D4-3 T5) ─────────────────────────────────────
  let llmProviders = $state([]);
  let showProviderForm = $state(false);
  let providerForm = $state({ name: '', provider: 'anthropic', model: '', base_url: '', api_key: '', active: true });
  let providerSaving = $state(false);
  let providerTesting = $state({});   // name → {loading, result}
  let activeProviderName = $state(null);  // vault-backed active provider name
  let activating = $state({});           // name → bool

  const PROVIDER_OPTIONS = [
    { value: 'anthropic', label: 'Anthropic', defaultModel: 'claude-opus-4-5' },
    { value: 'openai',    label: 'OpenAI',    defaultModel: 'gpt-4o' },
    { value: 'gemini',    label: 'Gemini',     defaultModel: 'gemini-2.5-pro' },
    { value: 'ollama',    label: 'Ollama',     defaultModel: 'llama3' },
    { value: 'moonshot',  label: 'Moonshot',   defaultModel: 'moonshot-v1-128k' },
  ];

  // Local editable copy — updated on load, mutated by toggles/inputs
  let cfg = $state({
    bmp:        { enabled: false, addr: '' },
    bgp_ls:     { enabled: false, addr: '' },
    pcep:       { enabled: false, addr: '' },
    otlp:       { enabled: false, addr: '' },
    netflow:    { enabled: false, addr: '' },
    syslog_udp: { enabled: false, addr: '' },
    syslog_tcp: { enabled: false, addr: '' },
    snmp:       { enabled: false, addr: '' },
  });

  const RECEIVERS = [
    { key: 'bmp',        label: 'BMP',           hint: 'BGP Monitoring Protocol (TCP)',           proto: 'tcp' },
    { key: 'bgp_ls',     label: 'BGP-LS',         hint: 'BGP Link-State via GoBGP sidecar (TCP)', proto: 'tcp' },
    { key: 'pcep',       label: 'PCEP',            hint: 'Path Computation Element Protocol (TCP)', proto: 'tcp' },
    { key: 'otlp',       label: 'OTLP',            hint: 'OpenTelemetry spans (HTTP/proto)',        proto: 'http' },
    { key: 'netflow',    label: 'NetFlow/IPFIX',   hint: 'Flow telemetry (UDP, v9 + IPFIX)',        proto: 'udp' },
    { key: 'syslog_udp', label: 'Syslog UDP',      hint: 'Syslog receiver — UDP (RFC 5424)',        proto: 'udp' },
    { key: 'syslog_tcp', label: 'Syslog TCP',      hint: 'Syslog receiver — TCP (RFC 3195)',        proto: 'tcp' },
    { key: 'snmp',       label: 'SNMP Traps',      hint: 'SNMP trap receiver (UDP, v1/v2c/v3)',     proto: 'udp' },
  ];

  async function pollStatus() {
    try {
      const r = await fetch('/api/receivers/status');
      if (!r.ok) return;
      const body = await r.json();
      const map = {};
      for (const s of (body.receivers || [])) map[s.name] = s;
      liveStatus = map;
    } catch (_) {}
  }

  function stateColor(name) {
    const s = liveStatus[name];
    if (!s) return 'muted';
    if (s.state === 'listening') return 'green';
    if (s.state === 'port_conflict') return 'red';
    if (s.state === 'error') return 'red';
    if (s.state === 'disabled') return 'muted';
    return 'muted';
  }

  function stateLabel(name) {
    const s = liveStatus[name];
    if (!s) return '';
    return s.state.replace('_', ' ');
  }

  onMount(async () => {
    pollStatus();
    statusPollTimer = setInterval(pollStatus, 5000);
    fetch('/api/ai/config').then(r => r.ok ? r.json() : null).then(d => { if (d) aiCfg = d; }).catch(() => {});
    loadProviders();
    loadActiveProvider();
    try {
      const r = await fetch('/api/settings/streaming');
      if (!r.ok) throw new Error(await r.text());
      settings = await r.json();
      cfg = {
        bmp:        { enabled: settings.bmp.enabled,        addr: settings.bmp.addr },
        bgp_ls:     { enabled: settings.bgp_ls.enabled,     addr: settings.bgp_ls.addr },
        pcep:       { enabled: settings.pcep.enabled,       addr: settings.pcep.addr },
        otlp:       { enabled: settings.otlp.enabled,       addr: settings.otlp.addr },
        netflow:    { enabled: settings.netflow.enabled,    addr: settings.netflow.addr },
        syslog_udp: { enabled: settings.syslog_udp.enabled, addr: settings.syslog_udp.addr },
        syslog_tcp: { enabled: settings.syslog_tcp.enabled, addr: settings.syslog_tcp.addr },
        snmp:       { enabled: settings.snmp.enabled,       addr: settings.snmp.addr },
      };
    } catch (e) {
      toast(`Failed to load streaming settings: ${e.message}`, 'error');
    } finally {
      loading = false;
    }
    return () => clearInterval(statusPollTimer);
  });

  function markDirty() { dirty = true; }

  async function save() {
    saving = true;
    dirty  = false;
    try {
      const r = await fetch('/api/settings/streaming', {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          bmp:        { enabled: cfg.bmp.enabled,        addr: cfg.bmp.addr },
          bgp_ls:     { enabled: cfg.bgp_ls.enabled,     addr: cfg.bgp_ls.addr },
          pcep:       { enabled: cfg.pcep.enabled,       addr: cfg.pcep.addr },
          otlp:       { enabled: cfg.otlp.enabled,       addr: cfg.otlp.addr },
          netflow:    { enabled: cfg.netflow.enabled,    addr: cfg.netflow.addr },
          syslog_udp: { enabled: cfg.syslog_udp.enabled, addr: cfg.syslog_udp.addr },
          syslog_tcp: { enabled: cfg.syslog_tcp.enabled, addr: cfg.syslog_tcp.addr },
          snmp:       { enabled: cfg.snmp.enabled,       addr: cfg.snmp.addr },
        }),
      });
      const body = await r.json();
      if (!r.ok) throw new Error(body.message || r.statusText);
      toast(body.message, 'info');
    } catch (e) {
      toast(`Save failed: ${e.message}`, 'error');
      dirty = true;
    } finally {
      saving = false;
    }
  }

  async function testAi() {
    aiTesting = true;
    aiTestResult = null;
    try {
      const r = await fetch('/api/ai/test', { method: 'POST' });
      aiTestResult = await r.json();
    } catch (e) {
      aiTestResult = { ok: false, error: e.message };
    } finally {
      aiTesting = false;
    }
  }

  async function loadProviders() {
    try {
      const r = await fetch('/api/ai/providers');
      if (r.ok) llmProviders = await r.json();
    } catch (_) {}
  }

  async function loadActiveProvider() {
    try {
      const r = await fetch('/api/ai/providers/active');
      if (r.ok) {
        const d = await r.json();
        activeProviderName = d.active_provider ?? null;
      }
    } catch (_) {}
  }

  async function setActiveProvider(name) {
    activating = { ...activating, [name]: true };
    try {
      const r = await fetch('/api/ai/providers/activate', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      if (!r.ok) throw new Error(await r.text());
      activeProviderName = name;
      toast(`'${name}' is now the active AI provider`, 'success');
    } catch (e) {
      toast(`Failed to activate: ${e.message}`, 'error');
    } finally {
      activating = { ...activating, [name]: false };
    }
  }

  async function saveProvider() {
    providerSaving = true;
    try {
      const r = await fetch('/api/ai/providers', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(providerForm),
      });
      if (!r.ok) throw new Error(await r.text());
      toast('Provider saved', 'success');
      showProviderForm = false;
      providerForm = { name: '', provider: 'anthropic', model: '', base_url: '', api_key: '', active: true };
      await loadProviders();
    } catch (e) {
      toast(`Save failed: ${e.message}`, 'error');
    } finally {
      providerSaving = false;
    }
  }

  async function removeProvider(name) {
    try {
      const r = await fetch('/api/ai/providers/remove', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      if (!r.ok) throw new Error(await r.text());
      toast(`Removed ${name}`, 'info');
      await loadProviders();
    } catch (e) {
      toast(e.message, 'error');
    }
  }

  async function testProvider(name) {
    providerTesting = { ...providerTesting, [name]: { loading: true, result: null } };
    try {
      const r = await fetch('/api/ai/providers/test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const data = await r.json();
      providerTesting = { ...providerTesting, [name]: { loading: false, result: data } };
    } catch (e) {
      providerTesting = { ...providerTesting, [name]: { loading: false, result: { ok: false, error: e.message } } };
    }
  }

  function editProvider(p) {
    providerForm = { name: p.name, provider: p.provider, model: p.model, base_url: p.base_url || '', api_key: '', active: p.active };
    showProviderForm = true;
  }

  // ── D4-7 T7: Runtime config sections ─────────────────────────────────────
  let configSections = $state([]);
  let configLoading = $state(true);
  let expandedSection = $state(null);
  let sectionEdits = $state({});   // section → JSON string being edited
  let sectionSaving = $state({});  // section → boolean

  async function loadConfigSections() {
    configLoading = true;
    try {
      const r = await fetch('/api/settings');
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      configSections = data.sections || [];
    } catch (e) {
      toast(`Failed to load config sections: ${e.message}`, 'error');
    } finally {
      configLoading = false;
    }
  }

  async function expandSection(section) {
    if (expandedSection === section) { expandedSection = null; return; }
    expandedSection = section;
    try {
      const r = await fetch(`/api/settings/${section}`);
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      const val = data.value != null ? JSON.stringify(data.value, null, 2) : '';
      sectionEdits = { ...sectionEdits, [section]: val };
    } catch (e) {
      toast(`Failed to load ${section}: ${e.message}`, 'error');
    }
  }

  async function saveSection(section) {
    sectionSaving = { ...sectionSaving, [section]: true };
    try {
      const json = sectionEdits[section];
      if (!json || !json.trim()) throw new Error('Empty body');
      JSON.parse(json); // validate
      const r = await fetch(`/api/settings/${section}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: json,
      });
      if (!r.ok) throw new Error(await r.text());
      toast(`Saved ${section}`, 'success');
      await loadConfigSections();
    } catch (e) {
      toast(`Save ${section} failed: ${e.message}`, 'error');
    } finally {
      sectionSaving = { ...sectionSaving, [section]: false };
    }
  }

  async function exportAllSettings() {
    try {
      const r = await fetch('/api/settings/export', { method: 'POST' });
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url; a.download = 'bonsai-settings-export.json'; a.click();
      URL.revokeObjectURL(url);
      toast('Settings exported', 'success');
    } catch (e) {
      toast(`Export failed: ${e.message}`, 'error');
    }
  }

  onMount(() => { loadConfigSections(); });

  function discard() {
    if (!settings) return;
    cfg = {
      bmp:        { enabled: settings.bmp.enabled,        addr: settings.bmp.addr },
      bgp_ls:     { enabled: settings.bgp_ls.enabled,     addr: settings.bgp_ls.addr },
      pcep:       { enabled: settings.pcep.enabled,       addr: settings.pcep.addr },
      otlp:       { enabled: settings.otlp.enabled,       addr: settings.otlp.addr },
      netflow:    { enabled: settings.netflow.enabled,    addr: settings.netflow.addr },
      syslog_udp: { enabled: settings.syslog_udp.enabled, addr: settings.syslog_udp.addr },
      syslog_tcp: { enabled: settings.syslog_tcp.enabled, addr: settings.syslog_tcp.addr },
      snmp:       { enabled: settings.snmp.enabled,       addr: settings.snmp.addr },
    };
    dirty = false;
  }
</script>

<div class="settings-page">
  <div class="page-header">
    <h1>Settings</h1>
    <div class="header-actions">
      {#if dirty}
        <button class="btn-secondary" onclick={discard} disabled={saving}>Discard</button>
        <button class="btn-primary" onclick={save} disabled={saving}>
          {saving ? 'Saving…' : 'Save & Apply'}
        </button>
      {/if}
    </div>
  </div>

  {#if dirty}
    <div class="restart-banner">
      Unsaved changes — click Save &amp; Apply to apply live.
    </div>
  {/if}

  {#if loading}
    <p class="loading-msg">Loading…</p>
  {:else}
    <section class="section">
      <h2>Streaming Receivers</h2>
      <p class="section-desc">
        Enable or disable streaming protocol receivers and configure their listen addresses.
        All changes apply live — no process restart required. Status badges update every 5s.
      </p>

      <div class="receiver-grid">
        {#each RECEIVERS as r}
          {@const rcfg = cfg[r.key]}
          <div class="receiver-card" class:enabled={rcfg.enabled}>
            <div class="card-header">
              <div class="card-title-row">
                <span class="card-label">{r.label}</span>
                <span class="card-proto proto-{r.proto}">{r.proto.toUpperCase()}</span>
                {#if stateLabel(r.key)}
                  <span class="live-badge live-{stateColor(r.key)}">{stateLabel(r.key)}</span>
                {/if}
              </div>
              <label class="toggle" title="{rcfg.enabled ? 'Enabled' : 'Disabled'}">
                <input type="checkbox" bind:checked={rcfg.enabled}
                       onchange={() => { cfg = cfg; markDirty(); }} />
                <span class="slider"></span>
              </label>
            </div>
            <p class="card-hint">{r.hint}</p>
            <div class="card-addr-row">
              <label class="addr-label">
                {r.proto === 'udp' ? 'UDP' : r.proto === 'http' ? 'HTTP' : 'TCP'} address
              </label>
              <input
                class="addr-input"
                type="text"
                value={rcfg.addr}
                oninput={(e) => { rcfg.addr = e.target.value; cfg = cfg; markDirty(); }}
                placeholder="0.0.0.0:port"
                disabled={!rcfg.enabled}
              />
            </div>
          </div>
        {/each}
      </div>
    </section>
  {/if}

  {#if aiCfg}
    <section class="section">
      <h2>AI Investigations</h2>
      <p class="section-desc">
        Configure the AI provider used for automated investigation analysis.
        Set <code>{aiCfg.api_key_env}</code> in the server environment to enable.
      </p>
      <div class="ai-grid">
        <div class="ai-row"><span class="ai-label">Provider</span><span class="ai-value">{aiCfg.provider}</span></div>
        <div class="ai-row"><span class="ai-label">Model</span><span class="ai-value">{aiCfg.model}</span></div>
        <div class="ai-row">
          <span class="ai-label">API Key</span>
          <span class="ai-value">
            {#if aiCfg.has_api_key}
              <span class="key-set">set via <code>{aiCfg.api_key_env}</code></span>
            {:else}
              <span class="key-missing">not set — set <code>{aiCfg.api_key_env}</code> env var</span>
            {/if}
          </span>
        </div>
        <div class="ai-row"><span class="ai-label">Per-investigation budget</span><span class="ai-value">${aiCfg.per_investigation_budget_usd.toFixed(2)}</span></div>
        <div class="ai-row"><span class="ai-label">Daily budget</span><span class="ai-value">${aiCfg.daily_budget_usd.toFixed(2)}</span></div>
        <div class="ai-row"><span class="ai-label">Auto-investigate unmatched</span><span class="ai-value">{aiCfg.auto_investigate_unmatched ? 'enabled' : 'disabled'}</span></div>
      </div>
      <div class="ai-actions">
        <button class="btn-secondary" onclick={testAi} disabled={aiTesting || !aiCfg.has_api_key}>
          {aiTesting ? 'Testing…' : 'Test Connection'}
        </button>
        {#if aiTestResult}
          <span class="ai-test-result ai-test-{aiTestResult.ok ? 'ok' : 'err'}">
            {aiTestResult.ok ? `Connection OK (${aiTestResult.latency_ms ?? '?'}ms)` : `Failed: ${aiTestResult.error}`}
          </span>
        {/if}
      </div>
    </section>
  {/if}

  <section class="section">
    <div class="section-header-row">
      <div>
        <h2>LLM Providers</h2>
        <p class="section-desc">Manage API keys for AI providers. Keys are stored in the encrypted vault.</p>
      </div>
      <button class="btn-primary" onclick={() => { showProviderForm = !showProviderForm; }}>+ Add Provider</button>
    </div>

    {#if showProviderForm}
      <div class="provider-form">
        <div class="pf-row">
          <label>Name<input type="text" bind:value={providerForm.name} placeholder="e.g. prod-anthropic" /></label>
          <label>Provider
            <select bind:value={providerForm.provider} onchange={() => {
              const opt = PROVIDER_OPTIONS.find(o => o.value === providerForm.provider);
              if (opt && !providerForm.model) providerForm.model = opt.defaultModel;
            }}>
              {#each PROVIDER_OPTIONS as opt}
                <option value={opt.value}>{opt.label}</option>
              {/each}
            </select>
          </label>
        </div>
        <div class="pf-row">
          <label>Model<input type="text" bind:value={providerForm.model} placeholder="model name" /></label>
          <label>Base URL (optional)<input type="text" bind:value={providerForm.base_url} placeholder="https://..." /></label>
        </div>
        <div class="pf-row">
          <label>API Key<input type="password" bind:value={providerForm.api_key} placeholder="leave blank to keep existing" /></label>
          <label class="toggle-label">
            <input type="checkbox" bind:checked={providerForm.active} /> Active
          </label>
        </div>
        <div class="pf-actions">
          <button class="btn-primary" onclick={saveProvider} disabled={providerSaving || !providerForm.name.trim()}>
            {providerSaving ? 'Saving…' : 'Save'}
          </button>
          <button class="btn-secondary" onclick={() => { showProviderForm = false; }}>Cancel</button>
        </div>
      </div>
    {/if}

    {#if activeProviderName}
      <div class="active-provider-banner">
        Active provider: <strong>{activeProviderName}</strong>
      </div>
    {/if}
    {#if llmProviders.length > 0}
      <div class="provider-grid">
        {#each llmProviders as p (p.name)}
          <div class="provider-card" class:inactive={!p.active} class:is-active-provider={p.name === activeProviderName}>
            <div class="prov-header">
              <div>
                <span class="prov-name">{p.name}</span>
                <span class="prov-type">{p.provider}</span>
              </div>
              <div style="display:flex;gap:6px;align-items:center">
                {#if p.name === activeProviderName}
                  <span class="badge healthy">● active</span>
                {/if}
                <span class="badge {p.active ? 'healthy' : 'critical'}">{p.active ? 'enabled' : 'disabled'}</span>
              </div>
            </div>
            <div class="prov-detail">
              <span class="ai-label">Model</span><span class="ai-value">{p.model}</span>
            </div>
            {#if p.base_url}
              <div class="prov-detail">
                <span class="ai-label">Base URL</span><span class="ai-value">{p.base_url}</span>
              </div>
            {/if}
            <div class="prov-detail">
              <span class="ai-label">API Key</span>
              <span class="ai-value">{p.has_api_key ? '••••••••' : '<not set>'}</span>
            </div>
            <div class="prov-actions">
              <button class="btn-secondary btn-sm" onclick={() => editProvider(p)}>Edit</button>
              <button class="btn-secondary btn-sm" onclick={() => testProvider(p.name)} disabled={providerTesting[p.name]?.loading || !p.has_api_key}>
                {providerTesting[p.name]?.loading ? 'Testing…' : 'Test'}
              </button>
              {#if p.name !== activeProviderName}
                <button class="btn-primary btn-sm" onclick={() => setActiveProvider(p.name)} disabled={activating[p.name]}>
                  {activating[p.name] ? 'Activating…' : 'Set Active'}
                </button>
              {/if}
              <button class="btn-danger btn-sm" onclick={() => removeProvider(p.name)}>Remove</button>
              {#if providerTesting[p.name]?.result}
                <span class="ai-test-result ai-test-{providerTesting[p.name].result.ok ? 'ok' : 'err'}">
                  {providerTesting[p.name].result.ok ? `OK (${providerTesting[p.name].result.latency_ms ?? '?'}ms)` : providerTesting[p.name].result.error}
                </span>
              {/if}
            </div>
          </div>
        {/each}
      </div>
    {:else if !showProviderForm}
      <p class="muted-hint">No LLM providers configured yet. Click "+ Add Provider" to store one in the vault.</p>
    {/if}
  </section>

  <!-- D4-7 T7: Runtime Config Sections -->
  <section class="section">
    <div class="section-header-row">
      <div>
        <h2>Runtime Configuration</h2>
        <p class="section-desc">
          All tunables are stored in the database. Edit a section and click Save to persist.
          Changes take effect on next boot or hot-reload.
        </p>
      </div>
      <button class="btn-secondary" onclick={exportAllSettings}>Export All</button>
    </div>

    {#if configLoading}
      <p class="loading-msg">Loading config sections…</p>
    {:else}
      <div class="config-section-list">
        {#each configSections as s (s.section)}
          <div class="config-section-row">
            <button class="config-section-header" onclick={() => expandSection(s.section)}>
              <span class="config-section-name">{s.section.replace(/_/g, ' ')}</span>
              <div class="config-section-meta">
                <span class="badge {s.in_db ? 'healthy' : 'muted'}">{s.in_db ? 'DB' : 'default'}</span>
                <span class="expand-icon">{expandedSection === s.section ? '▾' : '▸'}</span>
              </div>
            </button>
            {#if expandedSection === s.section}
              <div class="config-section-body">
                <textarea
                  class="config-json-editor"
                  value={sectionEdits[s.section] || ''}
                  oninput={(e) => { sectionEdits = { ...sectionEdits, [s.section]: e.target.value }; }}
                  rows="10"
                  spellcheck="false"
                  placeholder="Enter JSON config for this section…"
                ></textarea>
                <div class="config-section-actions">
                  <button class="btn-primary btn-sm" onclick={() => saveSection(s.section)} disabled={sectionSaving[s.section]}>
                    {sectionSaving[s.section] ? 'Saving…' : 'Save'}
                  </button>
                </div>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </section>
</div>

<style>
  .settings-page {
    padding: 24px 28px;
    max-width: 900px;
  }

  .page-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 16px;
  }

  .page-header h1 {
    margin: 0;
    font-size: 1.4rem;
    font-weight: 600;
  }

  .header-actions {
    display: flex;
    gap: 8px;
  }

  .restart-banner {
    background: #7c3aed22;
    border: 1px solid #7c3aed;
    border-radius: 6px;
    padding: 8px 14px;
    font-size: 0.82rem;
    color: #a78bfa;
    margin-bottom: 20px;
  }

  .loading-msg {
    color: var(--color-muted, #6b7280);
    font-size: 0.9rem;
  }

  .section {
    margin-bottom: 32px;
  }

  .section h2 {
    font-size: 1rem;
    font-weight: 600;
    margin: 0 0 4px;
  }

  .section-desc {
    font-size: 0.82rem;
    color: var(--color-muted, #6b7280);
    margin: 0 0 16px;
  }

  .receiver-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: 14px;
  }

  .receiver-card {
    background: var(--color-surface, #1a1a2e);
    border: 1px solid var(--color-border, #2d2d44);
    border-radius: 8px;
    padding: 14px 16px;
    transition: border-color 0.15s;
  }

  .receiver-card.enabled {
    border-color: #4c6ef5;
  }

  .card-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    margin-bottom: 6px;
  }

  .card-title-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .card-label {
    font-weight: 600;
    font-size: 0.9rem;
  }

  .card-proto {
    font-size: 0.65rem;
    font-weight: 700;
    padding: 1px 5px;
    border-radius: 4px;
    letter-spacing: 0.04em;
  }

  .proto-tcp  { background: #1e3a5f; color: #60a5fa; }
  .proto-udp  { background: #1a3a2e; color: #34d399; }
  .proto-http { background: #3a2a1a; color: #f59e0b; }

  .live-badge {
    font-size: 0.6rem;
    font-weight: 700;
    padding: 1px 5px;
    border-radius: 4px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .live-green { background: #14532d; color: #4ade80; }
  .live-red   { background: #450a0a; color: #f87171; }
  .live-muted { background: #1f2937; color: #6b7280; }

  .card-hint {
    font-size: 0.75rem;
    color: var(--color-muted, #6b7280);
    margin: 0 0 10px;
  }

  .card-addr-row {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .addr-label {
    font-size: 0.72rem;
    color: var(--color-muted, #6b7280);
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .addr-input {
    font-family: monospace;
    font-size: 0.8rem;
    background: var(--color-bg, #111827);
    border: 1px solid var(--color-border, #2d2d44);
    border-radius: 4px;
    padding: 5px 8px;
    color: inherit;
    width: 100%;
    box-sizing: border-box;
  }

  .addr-input:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  /* Toggle switch */
  .toggle {
    position: relative;
    display: inline-block;
    width: 36px;
    height: 20px;
    flex-shrink: 0;
    cursor: pointer;
  }

  .toggle input {
    opacity: 0;
    width: 0;
    height: 0;
    position: absolute;
  }

  .slider {
    position: absolute;
    inset: 0;
    background: #374151;
    border-radius: 20px;
    transition: background 0.2s;
  }

  .slider::before {
    content: '';
    position: absolute;
    width: 14px;
    height: 14px;
    left: 3px;
    bottom: 3px;
    background: #fff;
    border-radius: 50%;
    transition: transform 0.2s;
  }

  .toggle input:checked + .slider { background: #4c6ef5; }
  .toggle input:checked + .slider::before { transform: translateX(16px); }

  .btn-primary, .btn-secondary {
    padding: 6px 16px;
    border-radius: 6px;
    font-size: 0.82rem;
    font-weight: 500;
    cursor: pointer;
    border: none;
    transition: opacity 0.15s;
  }

  .btn-primary {
    background: #4c6ef5;
    color: #fff;
  }

  .btn-secondary {
    background: var(--color-surface, #1a1a2e);
    border: 1px solid var(--color-border, #2d2d44);
    color: inherit;
  }

  .btn-primary:disabled, .btn-secondary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .ai-grid { display: flex; flex-direction: column; gap: 6px; margin-bottom: 14px; }
  .ai-row { display: flex; gap: 12px; align-items: baseline; font-size: 0.83rem; }
  .ai-label { width: 200px; flex-shrink: 0; color: var(--color-muted, #6b7280); text-transform: uppercase; font-size: 0.72rem; letter-spacing: 0.05em; }
  .ai-value { font-family: monospace; }
  .key-set { color: #4ade80; }
  .key-missing { color: #f87171; }
  .ai-actions { display: flex; align-items: center; gap: 12px; }
  .ai-test-result { font-size: 0.8rem; }
  .ai-test-ok { color: #4ade80; }
  .ai-test-err { color: #f87171; }

  .section-header-row { display: flex; justify-content: space-between; align-items: flex-start; gap: 12px; margin-bottom: 14px; }
  .section-header-row h2 { margin: 0 0 4px; }
  .section-header-row .section-desc { margin: 0; }

  .provider-form { background: var(--color-surface, #1a1a2e); border: 1px solid #4c6ef5; border-radius: 8px; padding: 16px; margin-bottom: 16px; }
  .pf-row { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; margin-bottom: 10px; }
  .pf-row label { display: flex; flex-direction: column; gap: 4px; font-size: 0.78rem; color: var(--color-muted, #6b7280); text-transform: uppercase; letter-spacing: 0.04em; }
  .pf-row input, .pf-row select { font-size: 0.84rem; padding: 6px 8px; border-radius: 4px; border: 1px solid var(--color-border, #2d2d44); background: var(--color-bg, #111827); color: inherit; font-family: monospace; }
  .pf-actions { display: flex; gap: 8px; margin-top: 6px; }
  .toggle-label { display: flex; align-items: center; gap: 6px; font-size: 0.84rem; cursor: pointer; }
  .toggle-label input { width: auto; }

  .provider-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(320px, 1fr)); gap: 14px; }
  .provider-card { background: var(--color-surface, #1a1a2e); border: 1px solid var(--color-border, #2d2d44); border-radius: 8px; padding: 14px 16px; }
  .provider-card.inactive { opacity: 0.6; }
  .provider-card.is-active-provider { border-color: #10b981; box-shadow: 0 0 0 1px #10b98133; }
  .active-provider-banner { background: #10b98118; border: 1px solid #10b98155; border-radius: 6px; padding: 8px 14px; font-size: 0.85rem; color: #10b981; margin-bottom: 12px; }
  .prov-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 10px; }
  .prov-name { font-weight: 700; font-size: 0.95rem; }
  .prov-type { font-size: 0.72rem; font-weight: 600; padding: 1px 6px; border-radius: 4px; background: #1e3a5f; color: #60a5fa; margin-left: 8px; }
  .prov-detail { display: flex; gap: 12px; font-size: 0.82rem; margin-bottom: 4px; }
  .prov-actions { display: flex; gap: 6px; align-items: center; margin-top: 10px; padding-top: 10px; border-top: 1px solid var(--color-border, #2d2d44); flex-wrap: wrap; }
  .btn-sm { padding: 3px 10px; font-size: 0.75rem; }
  .btn-danger { background: #7f1d1d; color: #fca5a5; border: 1px solid #f8717144; border-radius: 6px; padding: 3px 10px; font-size: 0.75rem; cursor: pointer; }
  .btn-danger:hover { background: #991b1b; }
  .muted-hint { color: var(--color-muted, #6b7280); font-size: 0.82rem; }

  /* D4-7 T7: Runtime config sections */
  .config-section-list { display: flex; flex-direction: column; gap: 2px; }
  .config-section-row { border: 1px solid var(--color-border, #2d2d44); border-radius: 6px; overflow: hidden; }
  .config-section-header {
    display: flex; justify-content: space-between; align-items: center;
    width: 100%; padding: 10px 14px; background: var(--color-surface, #1a1a2e);
    border: none; color: inherit; cursor: pointer; font-size: 0.88rem; text-align: left;
  }
  .config-section-header:hover { background: #1e2940; }
  .config-section-name { font-weight: 600; text-transform: capitalize; }
  .config-section-meta { display: flex; align-items: center; gap: 8px; }
  .expand-icon { font-size: 0.75rem; color: var(--color-muted, #6b7280); }
  .badge.healthy { background: #14532d; color: #4ade80; font-size: 0.65rem; font-weight: 700; padding: 1px 6px; border-radius: 4px; }
  .badge.muted { background: #1f2937; color: #6b7280; font-size: 0.65rem; font-weight: 700; padding: 1px 6px; border-radius: 4px; }
  .config-section-body { padding: 0 14px 14px; background: var(--color-surface, #1a1a2e); }
  .config-json-editor {
    width: 100%; box-sizing: border-box; font-family: monospace; font-size: 0.78rem;
    background: var(--color-bg, #111827); border: 1px solid var(--color-border, #2d2d44);
    border-radius: 4px; padding: 8px; color: inherit; resize: vertical; tab-size: 2;
  }
  .config-section-actions { display: flex; gap: 8px; margin-top: 8px; }
</style>
