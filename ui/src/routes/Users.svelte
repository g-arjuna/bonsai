<script>
  import { onMount } from 'svelte';

  let tab = $state('llm');

  // ── LLM providers ──
  let providers = $state([]);
  let llmLoading = $state(true);
  let llmError = $state('');
  let llmMsg = $state('');
  let currentAi = $state(null);

  let newProv = $state({ name: '', provider: 'anthropic', model: '', base_url: '', active: true, api_key: '' });
  let adding = $state(false);
  let testing = $state({});

  // ── RBAC / API keys ──
  const RBAC_NOTE = `RBAC and scoped API keys are enforced via the BONSAI_REQUIRE_AUTH and BONSAI_API_KEY environment variables.
Set BONSAI_REQUIRE_AUTH=1 to require bearer tokens on all API calls.
Scoped API keys: stored in vault under alias "apikey-<name>", resolved at request time.
LDAP: configure [auth.ldap] in bonsai.toml (server, bind_dn, search_base, group_filter).`;

  const PROVIDERS = [
    { value: 'anthropic',  label: 'Anthropic (Claude)',   default_model: 'claude-3-5-sonnet-20241022' },
    { value: 'openai',     label: 'OpenAI (GPT)',          default_model: 'gpt-4o' },
    { value: 'gemini',     label: 'Google Gemini',         default_model: 'gemini-1.5-flash' },
    { value: 'moonshot',   label: 'Moonshot (Kimi)',       default_model: 'moonshot-v1-8k' },
    { value: 'ollama',     label: 'Ollama (local)',        default_model: 'llama3' },
  ];

  onMount(async () => {
    await Promise.all([loadProviders(), loadCurrentAi()]);
  });

  async function loadProviders() {
    llmLoading = true;
    try {
      const r = await fetch('/api/ai/providers');
      if (!r.ok) throw new Error(await r.text());
      providers = await r.json();
      llmError = '';
    } catch (e) { llmError = e.message; }
    finally { llmLoading = false; }
  }

  async function loadCurrentAi() {
    try {
      const r = await fetch('/api/ai/config');
      if (r.ok) currentAi = await r.json();
    } catch {}
  }

  function applyProviderDefaults() {
    const p = PROVIDERS.find(p => p.value === newProv.provider);
    if (p && !newProv.model) newProv.model = p.default_model;
  }

  async function addProvider() {
    adding = true; llmMsg = '';
    try {
      const r = await fetch('/api/ai/providers', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(newProv),
      });
      if (!r.ok) throw new Error(await r.text());
      llmMsg = 'Provider saved';
      newProv = { name: '', provider: 'anthropic', model: '', base_url: '', active: true, api_key: '' };
      await loadProviders();
    } catch (e) { llmMsg = e.message; }
    finally { adding = false; }
  }

  async function removeProvider(name) {
    try {
      await fetch('/api/ai/providers/remove', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      await loadProviders();
    } catch {}
  }

  async function testProvider(name) {
    testing = { ...testing, [name]: 'testing' };
    try {
      const r = await fetch('/api/ai/providers/test', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      });
      const res = await r.json();
      testing = { ...testing, [name]: res.ok ? 'ok' : (res.error ?? 'fail') };
    } catch (e) {
      testing = { ...testing, [name]: e.message };
    }
  }

  function activeIcon(p) { return p.active ? '◉' : '◎'; }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Security &amp; Configuration</p>
      <h2>Users &amp; Access</h2>
    </div>
  </div>

  <div class="tab-bar">
    {#each [['llm', 'LLM Providers'], ['rbac', 'Auth &amp; RBAC'], ['apikeys', 'API Keys']] as [t, label]}
      <button class="tab-btn" class:active={tab === t} onclick={() => (tab = t)}>
        {@html label}
      </button>
    {/each}
  </div>

  <!-- ── LLM Providers (D4-3 T5) ── -->
  {#if tab === 'llm'}
    {#if currentAi}
      <div class="active-banner">
        <span class="active-label">Active provider</span>
        <code>{currentAi.provider}</code>
        <span class="sep">·</span>
        <code>{currentAi.model}</code>
        {#if currentAi.has_api_key}
          <span class="key-ok">key set</span>
        {:else}
          <span class="key-missing">no key</span>
        {/if}
      </div>
    {/if}

    <div class="form-section">
      <h3>Add / Update Provider</h3>
      <div class="prov-form">
        <label>
          Name (alias)
          <input bind:value={newProv.name} placeholder="my-claude" />
        </label>
        <label>
          Provider
          <select bind:value={newProv.provider} onchange={applyProviderDefaults}>
            {#each PROVIDERS as p}
              <option value={p.value}>{p.label}</option>
            {/each}
          </select>
        </label>
        <label>
          Model
          <input class="mono-input" bind:value={newProv.model} placeholder="claude-3-5-sonnet-20241022" />
        </label>
        <label>
          Base URL <span class="opt">(optional, for Ollama/proxy)</span>
          <input class="mono-input" bind:value={newProv.base_url} placeholder="http://localhost:11434" />
        </label>
        <label>
          API Key <span class="opt">(stored in vault, never in TOML)</span>
          <input type="password" bind:value={newProv.api_key} placeholder="sk-…" autocomplete="off" />
        </label>
        <label class="checkbox-row">
          <input type="checkbox" bind:checked={newProv.active} />
          Set as active provider
        </label>
        <button class="primary" onclick={addProvider} disabled={adding || !newProv.name || !newProv.model}>
          {adding ? 'Saving…' : 'Save Provider'}
        </button>
      </div>
      {#if llmMsg}<p class="msg">{llmMsg}</p>{/if}
    </div>

    {#if llmLoading}
      <p class="muted">Loading…</p>
    {:else if llmError}
      <p class="error-msg">{llmError}</p>
    {:else if providers.length === 0}
      <p class="muted">No LLM providers configured. Add one above.</p>
    {:else}
      <table class="data-table">
        <thead>
          <tr><th>Name</th><th>Provider</th><th>Model</th><th>Key</th><th>Active</th><th></th></tr>
        </thead>
        <tbody>
          {#each providers as p}
            {@const testResult = testing[p.name]}
            <tr class:active-row={p.active}>
              <td><code>{p.name}</code></td>
              <td>{p.provider}</td>
              <td><code class="small-mono">{p.model}</code></td>
              <td>
                {#if p.has_api_key}
                  <span class="key-ok">✓ set</span>
                {:else}
                  <span class="key-missing">not set</span>
                {/if}
              </td>
              <td>{#if p.active}<span class="active-pill">active</span>{/if}</td>
              <td class="actions-cell">
                <button class="ghost-sm" onclick={() => testProvider(p.name)} disabled={testResult === 'testing'}>
                  {testResult === 'testing' ? '…' : 'Test'}
                </button>
                {#if testResult && testResult !== 'testing'}
                  <span class="test-result" class:ok={testResult === 'ok'} class:fail={testResult !== 'ok'}>
                    {testResult === 'ok' ? '✓' : '✗ ' + testResult.slice(0, 30)}
                  </span>
                {/if}
                <button class="ghost-sm danger" onclick={() => removeProvider(p.name)}>Remove</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

  <!-- ── Auth & RBAC (D4-3 T2 / T3) ── -->
  {:else if tab === 'rbac'}
    <div class="info-section">
      <h3>Authentication Model</h3>
      <p class="muted small">
        Bonsai uses environment-variable-gated auth. Set <code>BONSAI_REQUIRE_AUTH=1</code> to enforce
        bearer token validation on all API calls. JWT signing key is set via <code>BONSAI_JWT_SECRET</code>.
      </p>
      <div class="config-grid">
        <div class="config-item">
          <span class="config-key">BONSAI_REQUIRE_AUTH</span>
          <span class="config-desc">Enforce bearer token on all API routes (default: off)</span>
        </div>
        <div class="config-item">
          <span class="config-key">BONSAI_JWT_SECRET</span>
          <span class="config-desc">HS256 signing secret for JWT tokens</span>
        </div>
        <div class="config-item">
          <span class="config-key">BONSAI_ADMIN_USER / BONSAI_ADMIN_PASS</span>
          <span class="config-desc">Bootstrap admin credentials (first-run only)</span>
        </div>
      </div>
    </div>

    <div class="info-section">
      <h3>LDAP / Active Directory</h3>
      <p class="muted small">Configure in <code>bonsai.toml</code> under <code>[auth.ldap]</code>:</p>
      <pre class="code-block">
[auth.ldap]
enabled      = true
server       = "ldap://dc1.corp.example.com:389"
bind_dn      = "CN=svc-bonsai,OU=ServiceAccounts,DC=corp,DC=example,DC=com"
bind_pass_env = "BONSAI_LDAP_PASS"
search_base  = "OU=Users,DC=corp,DC=example,DC=com"
group_filter = "(&(objectClass=group)(cn=bonsai-*))"
admin_group  = "bonsai-admins"
readonly_group = "bonsai-readonly"</pre>
      <p class="muted small" style="margin-top:8px">
        Users in <code>bonsai-admins</code> get full write access. Users in <code>bonsai-readonly</code>
        get read-only access. All other authenticated users are denied.
      </p>
    </div>

    <div class="info-section">
      <h3>Roles</h3>
      <table class="data-table">
        <thead><tr><th>Role</th><th>Permissions</th></tr></thead>
        <tbody>
          <tr><td><code>admin</code></td><td>Full read/write, user management, vault re-key</td></tr>
          <tr><td><code>operator</code></td><td>Read + approve remediations, create investigations</td></tr>
          <tr><td><code>readonly</code></td><td>GET endpoints only, no mutations</td></tr>
          <tr><td><code>api</code></td><td>Scoped API key — see API Keys tab</td></tr>
        </tbody>
      </table>
    </div>

  <!-- ── Scoped API Keys (D4-3 T6) ── -->
  {:else if tab === 'apikeys'}
    <div class="info-section">
      <h3>Scoped API Keys</h3>
      <p class="muted small">
        API keys are stored in the vault under the alias <code>apikey-&lt;name&gt;</code>.
        Create keys via the vault CLI or the re-key API:
      </p>
      <pre class="code-block">
# Create a new scoped key (example via vault CLI)
BONSAI_VAULT_PASSPHRASE=... ./vault-rekey --add-apikey monitoring-system

# Use the key in requests
curl -H "Authorization: Bearer &lt;key&gt;" http://bonsai:8080/api/incidents</pre>
      <p class="muted small" style="margin-top:8px">
        Keys can be restricted to specific API path prefixes by setting a <code>scope</code> field
        (e.g. <code>scope=/api/incidents,/api/devices</code>) in the vault entry metadata.
        This is enforced by the JWT middleware when <code>BONSAI_REQUIRE_AUTH=1</code>.
      </p>

      <h3 style="margin-top:20px">Vault Re-Key</h3>
      <p class="muted small">
        Change the vault passphrase via the API (requires existing valid credentials):
      </p>
      <pre class="code-block">{'POST /api/vault/rekey\n{"new_passphrase_env": "BONSAI_VAULT_NEW_PASSPHRASE"}\n# Set the env var before calling, then restart with the new passphrase'}</pre>
    </div>
  {/if}
</div>

<style>
  .tab-bar { display: flex; gap: 4px; border-bottom: 1px solid var(--border-subtle); margin-bottom: 20px; }
  .tab-btn {
    padding: 7px 14px; background: none; border: none; border-bottom: 2px solid transparent;
    color: var(--text-secondary); cursor: pointer; font-size: 13px; font-family: inherit;
    transition: color 0.15s, border-color 0.15s;
  }
  .tab-btn:hover { color: var(--text-primary); }
  .tab-btn.active { color: var(--accent-primary, #58a6ff); border-bottom-color: var(--accent-primary, #58a6ff); }

  .active-banner {
    display: flex; align-items: center; gap: 8px;
    background: rgba(88,166,255,0.08); border: 1px solid rgba(88,166,255,0.2);
    border-radius: 6px; padding: 8px 14px; margin-bottom: 16px; font-size: 12px;
  }
  .active-label { font-size: 10px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-tertiary); }
  .sep { color: var(--text-tertiary); }

  .form-section { background: var(--bg-surface); border: 1px solid var(--border-subtle); border-radius: 6px; padding: 16px 20px; margin-bottom: 16px; }
  .form-section h3 { margin: 0 0 12px; font-size: 13px; font-weight: 600; }

  .prov-form { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 10px; align-items: end; }
  .prov-form label { display: flex; flex-direction: column; gap: 4px; font-size: 12px; color: var(--text-secondary); }
  .prov-form input, .prov-form select {
    padding: 5px 8px; background: var(--bg-elevated); border: 1px solid var(--border-subtle);
    border-radius: 4px; color: var(--text-primary); font-size: 12px; font-family: inherit;
  }
  .checkbox-row { flex-direction: row !important; align-items: center; gap: 8px !important; }
  .opt { font-size: 10px; color: var(--text-tertiary); font-weight: normal; }
  .mono-input { font-family: var(--font-mono); }

  .data-table { width: 100%; border-collapse: collapse; font-size: 12px; margin-bottom: 4px; }
  .data-table th { text-align: left; padding: 6px 10px; border-bottom: 1px solid var(--border-subtle); font-size: 11px; text-transform: uppercase; color: var(--text-tertiary); font-weight: 600; }
  .data-table td { padding: 6px 10px; border-bottom: 1px solid var(--border-subtle); color: var(--text-secondary); vertical-align: middle; }
  .active-row td { background: rgba(88,166,255,0.04); }

  .active-pill { font-size: 10px; background: rgba(88,166,255,0.12); color: #58a6ff; border: 1px solid rgba(88,166,255,0.25); padding: 1px 6px; border-radius: 3px; font-weight: 600; }
  .key-ok { color: var(--state-healthy, #22c55e); font-size: 11px; }
  .key-missing { color: var(--text-tertiary); font-size: 11px; }

  .actions-cell { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; }
  .ghost-sm { background: none; border: 1px solid var(--border-subtle); border-radius: 4px; padding: 2px 8px; font-size: 11px; cursor: pointer; color: var(--text-tertiary); font-family: inherit; }
  .ghost-sm:hover { color: var(--text-primary); border-color: var(--border-default); }
  .ghost-sm.danger:hover { color: #fca5a5; border-color: rgba(239,68,68,0.4); }
  .ghost-sm:disabled { opacity: 0.5; cursor: default; }

  .test-result { font-size: 11px; max-width: 200px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .test-result.ok { color: var(--state-healthy, #22c55e); }
  .test-result.fail { color: #fca5a5; }

  .small-mono { font-family: var(--font-mono); font-size: 11px; }

  .info-section { background: var(--bg-surface); border: 1px solid var(--border-subtle); border-radius: 6px; padding: 16px 20px; margin-bottom: 16px; }
  .info-section h3 { margin: 0 0 10px; font-size: 13px; font-weight: 600; }

  .config-grid { display: flex; flex-direction: column; gap: 6px; margin-top: 10px; }
  .config-item { display: flex; align-items: baseline; gap: 16px; font-size: 12px; padding: 4px 0; border-bottom: 1px solid var(--border-subtle); }
  .config-key { font-family: var(--font-mono); font-size: 11px; color: var(--accent-primary, #58a6ff); width: 260px; flex-shrink: 0; }
  .config-desc { color: var(--text-secondary); }

  .code-block {
    background: var(--bg-elevated); border: 1px solid var(--border-subtle); border-radius: 4px;
    padding: 10px 14px; font-size: 11px; font-family: var(--font-mono); color: var(--text-secondary);
    white-space: pre-wrap; overflow-x: auto; margin: 8px 0 0;
  }

  .msg { font-size: 12px; color: var(--text-secondary); margin-top: 6px; }
  .muted { color: var(--text-tertiary); }
  .small { font-size: 11px; }
  .error-msg { color: #fca5a5; font-size: 12px; }
</style>
