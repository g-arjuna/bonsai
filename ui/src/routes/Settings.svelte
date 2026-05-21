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
            {aiTestResult.ok ? 'Connection OK' : `Failed: ${aiTestResult.error}`}
          </span>
        {/if}
      </div>
    </section>
  {/if}
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
</style>
