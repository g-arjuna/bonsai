<script>
  import { onMount } from 'svelte';

  let certs = $state([]);
  let loading = $state(true);
  let error = $state(null);

  // Add form state
  let showForm = $state(false);
  let form = $state({ name: '', label: '', pem: '', role: '' });
  let saving = $state(false);
  let saveError = $state(null);
  let saveOk = $state(false);

  // Copy / download state keyed by cert name
  let downloading = $state({});

  // Verify tool
  let verifyPath = $state('');
  let verifyLoading = $state(false);
  let verifyResult = $state(null);

  async function verifyPathCheck() {
    verifyLoading = true;
    verifyResult = null;
    try {
      const r = await fetch('/api/certs/verify', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ path: verifyPath }),
      });
      verifyResult = await r.json();
    } catch (e) {
      verifyResult = { ok: false, error: e.message };
    } finally {
      verifyLoading = false;
    }
  }

  async function load() {
    loading = true;
    error = null;
    try {
      const r = await fetch('/api/certs');
      if (!r.ok) throw new Error(await r.text());
      const d = await r.json();
      certs = d.certs ?? [];
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function save() {
    saving = true;
    saveError = null;
    saveOk = false;
    try {
      const r = await fetch('/api/certs', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ name: form.name, label: form.label, pem: form.pem, role: form.role }),
      });
      if (!r.ok) throw new Error(await r.text());
      saveOk = true;
      form = { name: '', label: '', pem: '', role: '' };
      showForm = false;
      await load();
    } catch (e) {
      saveError = e.message;
    } finally {
      saving = false;
    }
  }

  async function deleteCert(name) {
    if (!confirm(`Remove cert "${name}" from vault?`)) return;
    try {
      const r = await fetch('/api/certs/' + encodeURIComponent(name), { method: 'DELETE' });
      if (!r.ok && r.status !== 204) throw new Error(await r.text());
      await load();
    } catch (e) {
      alert('Delete failed: ' + e.message);
    }
  }

  async function downloadPem(name) {
    downloading = { ...downloading, [name]: true };
    try {
      const r = await fetch('/api/certs/' + encodeURIComponent(name));
      if (!r.ok) throw new Error(await r.text());
      const pem = await r.text();
      const blob = new Blob([pem], { type: 'application/x-pem-file' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = name + '.pem';
      a.click();
      URL.revokeObjectURL(url);
    } catch (e) {
      alert('Download failed: ' + e.message);
    } finally {
      downloading = { ...downloading, [name]: false };
    }
  }

  function handleFileInput(e) {
    const file = e.target.files?.[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = (ev) => {
      form.pem = ev.target.result;
      if (!form.name) form.name = file.name.replace(/\.(pem|crt|cer)$/i, '');
    };
    reader.readAsText(file);
  }

  // Apply-to-service state
  let appliedConfig = $state({});
  let applyForm = $state({ target: 'http_tls', ca_cert: '', cert: '', key: '', restart: false });
  let applyLoading = $state(false);
  let applyResult = $state(null);
  let showApply = $state(false);

  async function loadApplied() {
    try {
      const r = await fetch('/api/certs/applied');
      if (r.ok) appliedConfig = await r.json();
    } catch (_) {}
  }

  async function applyToService() {
    applyLoading = true;
    applyResult = null;
    try {
      const body = { target: applyForm.target, restart: applyForm.restart };
      if (applyForm.ca_cert) body.ca_cert = applyForm.ca_cert;
      if (applyForm.cert) body.cert = applyForm.cert;
      if (applyForm.key) body.key = applyForm.key;
      const r = await fetch('/api/certs/apply', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      const d = await r.json();
      if (!r.ok) throw new Error(d.error ?? JSON.stringify(d));
      applyResult = { ok: true, ...d };
      await loadApplied();
    } catch (e) {
      applyResult = { ok: false, error: e.message };
    } finally {
      applyLoading = false;
    }
  }

  function fmtTime(ns) {
    if (!ns) return '—';
    return new Date(ns / 1_000_000).toLocaleString();
  }

  function fmtExpiry(secs) {
    if (!secs) return null;
    const d = new Date(secs * 1000);
    const daysLeft = Math.floor((d - Date.now()) / 86400000);
    return { date: d.toLocaleDateString(), daysLeft };
  }

  function shortFp(fp) {
    if (!fp) return '—';
    return fp.slice(0, 8) + '…' + fp.slice(-8);
  }

  const ROLE_LABELS = {
    ca: '🔐 CA',
    server_cert: '🖥️ Server cert',
    server_key: '🔑 Server key',
    client_cert: '📋 Client cert',
    client_key: '🗝️ Client key',
  };

  const TARGET_FIELDS = {
    http_tls:     { cert: true, key: true, ca_cert: false },
    runtime_mtls: { cert: true, key: true, ca_cert: true },
    gnmi_ca:      { cert: false, key: false, ca_cert: true },
  };

  onMount(() => { load(); loadApplied(); });
</script>

<div class="page">
  <div class="page-header">
    <div>
      <h1>TLS Certificates</h1>
      <p class="subtitle">Vault-stored CA certs and client certs. Reference them by name as <code>ca_cert</code> in device configs. PEM content is encrypted at rest.</p>
    </div>
    <div class="header-actions">
      <button class="btn-secondary" onclick={load}>Refresh</button>
      <button class="btn-primary" onclick={() => { showForm = !showForm; saveError = null; saveOk = false; }}>
        {showForm ? 'Cancel' : '+ Add Cert'}
      </button>
    </div>
  </div>

  {#if showForm}
    <div class="add-form">
      <h3>Add / Replace Certificate</h3>
      <div class="form-grid">
        <label>
          Name <span class="required">*</span>
          <input type="text" bind:value={form.name} placeholder="srl-lab-ca" />
          <span class="hint">Used as the vault alias key (<code>cert-{form.name || 'name'}</code>)</span>
        </label>
        <label>
          Label
          <input type="text" bind:value={form.label} placeholder="SRL Lab CA Certificate" />
        </label>
        <label>
          Role <span class="hint-inline">(optional, for enterprise CA workflows)</span>
          <select bind:value={form.role}>
            <option value="">— unset —</option>
            <option value="ca">CA certificate</option>
            <option value="server_cert">Server certificate (chain)</option>
            <option value="server_key">Server private key</option>
            <option value="client_cert">Client certificate</option>
            <option value="client_key">Client private key</option>
          </select>
        </label>
      </div>
      <label class="pem-label">
        PEM content <span class="required">*</span>
        <div class="pem-row">
          <textarea
            class="pem-area"
            bind:value={form.pem}
            placeholder="-----BEGIN CERTIFICATE-----&#10;...&#10;-----END CERTIFICATE-----"
            rows="8"
          ></textarea>
          <label class="file-btn" title="Upload .pem / .crt file">
            📂 Upload
            <input type="file" accept=".pem,.crt,.cer" onchange={handleFileInput} style="display:none" />
          </label>
        </div>
      </label>
      {#if saveError}
        <div class="banner-error">{saveError}</div>
      {/if}
      {#if saveOk}
        <div class="banner-ok">Certificate stored in vault.</div>
      {/if}
      <div class="form-actions">
        <button class="btn-primary" onclick={save} disabled={saving || !form.name || !form.pem}>
          {saving ? 'Storing…' : 'Store in Vault'}
        </button>
      </div>
    </div>
  {/if}

  {#if loading}
    <div class="loading">Loading certificates…</div>
  {:else if error}
    <div class="error">Error: {error}</div>
  {:else if certs.length === 0}
    <div class="empty">
      No certificates stored yet. Click <strong>+ Add Cert</strong> to upload a CA or client certificate PEM.
    </div>
  {:else}
    <table>
      <thead>
        <tr>
          <th>Name</th>
          <th>Label / Role</th>
          <th>Fingerprint (SHA-256)</th>
          <th>Expiry</th>
          <th>Added</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each certs as c (c.name)}
          {@const exp = fmtExpiry(c.expires_at)}
          <tr class={exp && exp.daysLeft < 30 ? 'expiry-warn' : exp && exp.daysLeft < 0 ? 'expiry-expired' : ''}>
            <td><code class="cert-name">{c.name}</code></td>
            <td>
              <span>{c.label || c.name}</span>
              {#if c.role}<span class="role-badge">{ROLE_LABELS[c.role] ?? c.role}</span>{/if}
            </td>
            <td>
              <span class="fp" title={c.fingerprint_sha256}>{shortFp(c.fingerprint_sha256)}</span>
            </td>
            <td class="expiry-cell">
              {#if exp}
                <span class={exp.daysLeft < 0 ? 'expiry-red' : exp.daysLeft < 30 ? 'expiry-amber' : 'expiry-green'}>
                  {exp.date}
                  {#if exp.daysLeft < 0}
                    <span class="expiry-tag">EXPIRED</span>
                  {:else if exp.daysLeft < 30}
                    <span class="expiry-tag">{exp.daysLeft}d</span>
                  {:else}
                    <span class="expiry-dim">{exp.daysLeft}d</span>
                  {/if}
                </span>
              {:else}
                <span class="dim">—</span>
              {/if}
            </td>
            <td class="dim">{fmtTime(c.added_at_ns)}</td>
            <td class="actions-cell">
              <button class="btn-ghost" onclick={() => downloadPem(c.name)} disabled={downloading[c.name]} title="Download PEM">
                {downloading[c.name] ? '…' : '⬇'}
              </button>
              <button class="btn-danger-sm" onclick={() => deleteCert(c.name)} title="Remove from vault">✕</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    <div class="row-count">{certs.length} certificate{certs.length !== 1 ? 's' : ''}</div>
  {/if}

  <div class="apply-panel">
    <div class="apply-header" onclick={() => showApply = !showApply} role="button" tabindex="0">
      <h4>Apply to Service {showApply ? '▲' : '▼'}</h4>
      <span class="muted apply-subtitle">Activate a cert bundle for HTTPS, mTLS, or gNMI — saves to DB, optionally restarts</span>
    </div>

    {#if Object.keys(appliedConfig).length > 0}
      <div class="applied-summary">
        {#each Object.entries(appliedConfig) as [target, fields]}
          <div class="applied-row">
            <span class="applied-target">{target}</span>
            {#each Object.entries(fields) as [field, val]}
              <span class="applied-item"><span class="applied-field">{field}</span> <code>{val}</code></span>
            {/each}
          </div>
        {/each}
      </div>
    {/if}

    {#if showApply}
      <div class="apply-form">
        <div class="apply-target-row">
          <label>Target service
            <select bind:value={applyForm.target}>
              <option value="http_tls">HTTPS API server (HTTP → HTTPS)</option>
              <option value="runtime_mtls">gRPC mTLS — core ↔ collectors</option>
              <option value="gnmi_ca">gNMI default CA (new device subscriptions)</option>
            </select>
          </label>
        </div>
        {#if TARGET_FIELDS[applyForm.target]?.ca_cert}
          <label class="apply-field-label">CA cert (vault name)
            <select bind:value={applyForm.ca_cert}>
              <option value="">— none —</option>
              {#each certs.filter(c => !c.role || c.role === 'ca') as c}
                <option value={c.name}>{c.name}{c.label ? ' — ' + c.label : ''}</option>
              {/each}
            </select>
          </label>
        {/if}
        {#if TARGET_FIELDS[applyForm.target]?.cert}
          <label class="apply-field-label">Server/client cert (vault name)
            <select bind:value={applyForm.cert}>
              <option value="">— none —</option>
              {#each certs.filter(c => !c.role || c.role === 'server_cert' || c.role === 'client_cert') as c}
                <option value={c.name}>{c.name}{c.label ? ' — ' + c.label : ''}</option>
              {/each}
            </select>
          </label>
        {/if}
        {#if TARGET_FIELDS[applyForm.target]?.key}
          <label class="apply-field-label">Private key (vault name)
            <select bind:value={applyForm.key}>
              <option value="">— none —</option>
              {#each certs.filter(c => !c.role || c.role === 'server_key' || c.role === 'client_key') as c}
                <option value={c.name}>{c.name}{c.label ? ' — ' + c.label : ''}</option>
              {/each}
            </select>
          </label>
        {/if}
        <label class="restart-label">
          <input type="checkbox" bind:checked={applyForm.restart} />
          Restart bonsai after apply (systemd/docker will auto-restart)
        </label>
        {#if applyResult}
          <div class="apply-result {applyResult.ok ? 'ok' : 'fail'}">
            {#if applyResult.ok}
              ✓ {applyResult.message}
              {#if applyResult.applied?.length}
                <ul class="applied-list">{#each applyResult.applied as a}<li><code>{a}</code></li>{/each}</ul>
              {/if}
            {:else}
              ✗ {applyResult.error}
            {/if}
          </div>
        {/if}
        <div class="apply-actions">
          <button class="btn-primary" onclick={applyToService} disabled={applyLoading}>
            {applyLoading ? 'Applying…' : 'Apply & Save'}
          </button>
          {#if applyForm.restart}
            <span class="restart-warn">⚠ Service will restart — UI will be briefly unavailable</span>
          {/if}
        </div>
      </div>
    {/if}
  </div>

  <div class="verify-tool">
    <h4>Verify cert path</h4>
    <p class="muted">Check that a cert path (file or <code>vault:name</code>) is reachable from the server. Useful for troubleshooting gNMI TLS errors.</p>
    <div class="verify-row">
      <input
        class="verify-input"
        type="text"
        placeholder="vault:srl-lab-ca  or  lab/fast-iteration/ca.pem"
        bind:value={verifyPath}
      />
      <button class="btn-secondary" onclick={verifyPathCheck} disabled={verifyLoading || !verifyPath}>
        {verifyLoading ? 'Checking…' : 'Check'}
      </button>
    </div>
    {#if verifyResult}
      <div class="verify-result {verifyResult.ok ? 'ok' : 'fail'}">
        {#if verifyResult.ok}
          ✓ Reachable via <strong>{verifyResult.source}</strong>
        {:else}
          ✗ {verifyResult.error}
        {/if}
      </div>
    {/if}
  </div>

  <div class="usage-hint">
    <h4>Integration with gNMI &amp; mTLS</h4>
    <div class="usage-grid">
      <div class="usage-item">
        <div class="usage-icon">📡</div>
        <div>
          <strong>gNMI device subscriptions</strong>
          <p>Set <code>ca_cert = "vault:srl-lab-ca"</code> in the device config (or onboarding wizard CA cert path field). The subscriber resolves it from vault at connect time.</p>
        </div>
      </div>
      <div class="usage-item">
        <div class="usage-icon">🔍</div>
        <div>
          <strong>gNMI discovery &amp; readiness</strong>
          <p>Discovery and gNMI readiness checks also resolve <code>vault:</code> references automatically — no file copy needed on the ops box.</p>
        </div>
      </div>
      <div class="usage-item">
        <div class="usage-icon">🔧</div>
        <div>
          <strong>gNMI Set / remediation</strong>
          <p>Playbook-driven gNMI Set operations use the same vault-aware resolver, so remediation targets can use vault cert refs.</p>
        </div>
      </div>
      <div class="usage-item">
        <div class="usage-icon">🔒</div>
        <div>
          <strong>mTLS collector ↔ core</strong>
          <p>Runtime mTLS (<code>runtime.tls.*</code> in bonsai.toml) still uses file paths — these are loaded once at startup. Store them via the file system and point bonsai.toml at the path.</p>
        </div>
      </div>
    </div>
    <p class="muted hint-note">Vault cert PEM is encrypted at rest with your vault passphrase. Cert references like <code>vault:name</code> work anywhere a <code>ca_cert</code> path is accepted.</p>
  </div>
</div>

<style>
  .page { padding: 24px 28px; max-width: 1100px; }
  .page-header { display: flex; justify-content: space-between; align-items: flex-start; margin-bottom: 20px; }
  h1 { font-size: 1.4rem; font-weight: 700; margin: 0 0 4px; }
  .subtitle { font-size: 0.82rem; color: var(--color-muted, #6b7280); margin: 0; }
  .header-actions { display: flex; gap: 8px; align-items: center; }

  .add-form {
    background: var(--color-surface, #1a1a2e);
    border: 1px solid var(--color-border, #2d2d44);
    border-radius: 10px;
    padding: 20px 22px;
    margin-bottom: 22px;
  }
  .add-form h3 { margin: 0 0 16px; font-size: 1rem; }
  .form-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; margin-bottom: 14px; }
  .form-grid label, .pem-label { display: flex; flex-direction: column; gap: 5px; font-size: 0.8rem; color: var(--color-muted, #6b7280); text-transform: uppercase; letter-spacing: 0.04em; }
  .form-grid input { font-size: 0.88rem; padding: 7px 10px; border-radius: 6px; border: 1px solid var(--color-border, #2d2d44); background: var(--color-bg, #111827); color: inherit; }
  .hint { font-size: 0.72rem; color: var(--color-muted, #6b7280); text-transform: none; letter-spacing: 0; }
  .required { color: #f87171; }

  .pem-label { margin-bottom: 14px; }
  .pem-row { display: flex; gap: 8px; align-items: flex-start; }
  .pem-area { flex: 1; font-family: monospace; font-size: 0.78rem; padding: 8px 10px; border-radius: 6px; border: 1px solid var(--color-border, #2d2d44); background: var(--color-bg, #111827); color: inherit; resize: vertical; }
  .file-btn { display: inline-flex; align-items: center; gap: 4px; padding: 7px 12px; border-radius: 6px; border: 1px solid var(--color-border, #2d2d44); background: var(--color-surface, #1a1a2e); cursor: pointer; font-size: 0.82rem; white-space: nowrap; color: inherit; text-transform: none; letter-spacing: 0; }
  .file-btn:hover { background: var(--color-border, #2d2d44); }

  .form-actions { display: flex; gap: 8px; margin-top: 4px; }
  .banner-error { background: #7f1d1d22; border: 1px solid #f8717144; color: #f87171; padding: 8px 12px; border-radius: 6px; font-size: 0.82rem; margin-bottom: 10px; }
  .banner-ok { background: #10b98118; border: 1px solid #10b98155; color: #10b981; padding: 8px 12px; border-radius: 6px; font-size: 0.82rem; margin-bottom: 10px; }

  table { width: 100%; border-collapse: collapse; font-size: 0.84rem; }
  th { text-align: left; padding: 7px 10px; border-bottom: 1px solid var(--color-border, #2d2d44); color: var(--color-muted, #6b7280); font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.05em; font-weight: 600; white-space: nowrap; }
  td { padding: 8px 10px; border-bottom: 1px solid var(--color-border, #2d2d4422); vertical-align: middle; }
  tr:hover td { background: var(--color-surface, #1a1a2e); }

  .cert-name { font-size: 0.85rem; }
  .fp { font-family: monospace; font-size: 0.78rem; color: var(--color-muted, #6b7280); }
  .dim { color: var(--color-muted, #6b7280); font-size: 0.8rem; }
  .actions-cell { display: flex; gap: 6px; align-items: center; }

  .role-badge { margin-left: 6px; font-size: 0.72rem; padding: 2px 6px; border-radius: 10px; background: #1e3a5f; color: #93c5fd; border: 1px solid #1d4ed866; }
  .hint-inline { font-size: 0.72rem; color: var(--color-muted, #6b7280); text-transform: none; letter-spacing: 0; }
  .form-grid select { font-size: 0.88rem; padding: 7px 10px; border-radius: 6px; border: 1px solid var(--color-border, #2d2d44); background: var(--color-bg, #111827); color: inherit; }

  .expiry-cell { font-size: 0.78rem; }
  .expiry-green { color: #10b981; }
  .expiry-amber { color: #f59e0b; }
  .expiry-red { color: #f87171; font-weight: 600; }
  .expiry-tag { font-size: 0.7rem; font-weight: 700; padding: 1px 5px; border-radius: 4px; margin-left: 4px; background: #f59e0b22; border: 1px solid #f59e0b44; }
  .expiry-red .expiry-tag { background: #f8717122; border-color: #f8717144; }
  .expiry-dim { color: var(--color-muted, #6b7280); font-size: 0.72rem; margin-left: 4px; }
  tr.expiry-warn td { background: #f59e0b08; }

  .apply-panel { margin-top: 24px; border: 1px solid var(--color-border, #2d2d44); border-radius: 8px; background: var(--color-surface, #1a1a2e); overflow: hidden; }
  .apply-header { display: flex; align-items: baseline; gap: 12px; padding: 12px 16px; cursor: pointer; user-select: none; }
  .apply-header:hover { background: #ffffff06; }
  .apply-header h4 { margin: 0; font-size: 0.9rem; }
  .apply-subtitle { font-size: 0.78rem; }
  .applied-summary { padding: 6px 16px 10px; display: flex; flex-direction: column; gap: 4px; border-top: 1px solid var(--color-border, #2d2d44); }
  .applied-row { display: flex; align-items: center; gap: 10px; font-size: 0.78rem; flex-wrap: wrap; }
  .applied-target { font-weight: 600; font-size: 0.75rem; padding: 2px 7px; border-radius: 4px; background: #1e40af22; color: #93c5fd; border: 1px solid #1e40af44; }
  .applied-item { display: flex; align-items: center; gap: 4px; }
  .applied-field { color: var(--color-muted, #6b7280); font-size: 0.72rem; }
  .apply-form { padding: 14px 16px; border-top: 1px solid var(--color-border, #2d2d44); display: flex; flex-direction: column; gap: 10px; }
  .apply-target-row label, .apply-field-label { display: flex; flex-direction: column; gap: 4px; font-size: 0.78rem; color: var(--color-muted, #6b7280); text-transform: uppercase; letter-spacing: 0.04em; }
  .apply-target-row select, .apply-field-label select { font-size: 0.84rem; padding: 6px 10px; border-radius: 6px; border: 1px solid var(--color-border, #2d2d44); background: var(--color-bg, #111827); color: inherit; text-transform: none; letter-spacing: 0; }
  .restart-label { display: flex; align-items: center; gap: 8px; font-size: 0.82rem; color: inherit; cursor: pointer; }
  .restart-warn { font-size: 0.78rem; color: #f59e0b; }
  .apply-actions { display: flex; align-items: center; gap: 12px; margin-top: 4px; }
  .apply-result { font-size: 0.82rem; padding: 8px 12px; border-radius: 6px; }
  .apply-result.ok { background: #10b98118; color: #10b981; border: 1px solid #10b98144; }
  .apply-result.fail { background: #7f1d1d22; color: #f87171; border: 1px solid #f8717144; }
  .applied-list { margin: 6px 0 0 16px; padding: 0; font-size: 0.78rem; }

  .loading, .empty, .error { padding: 40px; text-align: center; color: var(--color-muted, #6b7280); font-size: 0.88rem; }
  .error { color: #f87171; }
  .row-count { font-size: 0.75rem; color: var(--color-muted, #6b7280); margin-top: 10px; text-align: right; }

  .btn-primary { background: #1e40af; color: #fff; border: none; border-radius: 6px; padding: 7px 16px; cursor: pointer; font-size: 0.84rem; }
  .btn-primary:hover:not(:disabled) { background: #1d4ed8; }
  .btn-primary:disabled { opacity: 0.5; cursor: default; }
  .btn-secondary { background: var(--color-surface, #1a1a2e); border: 1px solid var(--color-border, #2d2d44); border-radius: 6px; padding: 6px 14px; cursor: pointer; color: inherit; font-size: 0.82rem; }
  .btn-secondary:hover { background: var(--color-border, #2d2d44); }
  .btn-ghost { background: none; border: 1px solid var(--color-border, #2d2d44); border-radius: 5px; padding: 3px 8px; cursor: pointer; color: inherit; font-size: 0.82rem; }
  .btn-ghost:hover:not(:disabled) { background: var(--color-border, #2d2d44); }
  .btn-danger-sm { background: #7f1d1d; color: #fca5a5; border: 1px solid #f8717144; border-radius: 5px; padding: 3px 8px; font-size: 0.78rem; cursor: pointer; }
  .btn-danger-sm:hover { background: #991b1b; }

  .verify-tool { margin-top: 28px; padding: 16px 18px; background: var(--color-surface, #1a1a2e); border: 1px solid var(--color-border, #2d2d44); border-radius: 8px; }
  .verify-tool h4 { margin: 0 0 6px; font-size: 0.9rem; }
  .verify-row { display: flex; gap: 8px; margin-top: 10px; }
  .verify-input { flex: 1; font-family: monospace; font-size: 0.84rem; padding: 7px 10px; border-radius: 6px; border: 1px solid var(--color-border, #2d2d44); background: var(--color-bg, #111827); color: inherit; }
  .verify-result { margin-top: 8px; font-size: 0.82rem; padding: 6px 10px; border-radius: 5px; }
  .verify-result.ok { background: #10b98118; color: #10b981; border: 1px solid #10b98144; }
  .verify-result.fail { background: #7f1d1d22; color: #f87171; border: 1px solid #f8717144; }

  .usage-hint { margin-top: 20px; padding: 16px 18px; background: var(--color-surface, #1a1a2e); border: 1px solid var(--color-border, #2d2d44); border-radius: 8px; }
  .usage-hint h4 { margin: 0 0 12px; font-size: 0.9rem; }
  .usage-hint p { margin: 0 0 6px; font-size: 0.82rem; }
  .usage-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; margin-bottom: 12px; }
  .usage-item { display: flex; gap: 10px; align-items: flex-start; }
  .usage-icon { font-size: 1.3rem; flex-shrink: 0; margin-top: 2px; }
  .usage-item strong { font-size: 0.84rem; display: block; margin-bottom: 3px; }
  .usage-item p { margin: 0; font-size: 0.78rem; color: var(--color-muted, #6b7280); }
  .hint-note { font-size: 0.78rem; }
  .muted { color: var(--color-muted, #6b7280); }
</style>
