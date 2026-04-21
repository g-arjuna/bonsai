<script>
  import { onMount } from 'svelte';

  const STEPS = [
    { id: 1, label: 'Identity' },
    { id: 2, label: 'Discovery' },
    { id: 3, label: 'Paths' },
    { id: 4, label: 'Confirm' }
  ];

  let loading = $state(true);
  let saving = $state(false);
  let discovering = $state(false);
  let workspace = $state('wizard');
  let step = $state(1);
  let error = $state('');
  let message = $state('');
  let devices = $state([]);
  let credentials = $state([]);
  let sites = $state([]);
  let vaultUnlocked = $state(false);
  let discovery = $state(null);
  let selectedProfileName = $state('');
  let selectedPathIds = $state([]);

  let form = $state(emptyForm());

  let credentialForm = $state({
    alias: '',
    username: '',
    password: ''
  });

  let siteForm = $state({
    name: '',
    kind: 'dc',
    parent_id: '',
    metadata_json: '{}'
  });

  function emptyForm() {
    return {
      address: '',
      hostname: '',
      vendor: '',
      role: 'leaf',
      site: 'lab',
      credential_alias: '',
      username_env: '',
      password_env: '',
      tls_domain: '',
      ca_cert: ''
    };
  }

  async function loadDevices() {
    try {
      const response = await fetch('/api/onboarding/devices');
      if (!response.ok) throw new Error(await response.text());
      const body = await response.json();
      devices = body.devices || [];
      error = '';
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function loadCredentials() {
    try {
      const response = await fetch('/api/credentials');
      if (!response.ok) throw new Error(await response.text());
      const body = await response.json();
      credentials = body.credentials || [];
      vaultUnlocked = !!body.unlocked;
    } catch (e) {
      error = e.message;
    }
  }

  async function loadSites() {
    try {
      const response = await fetch('/api/sites');
      if (!response.ok) throw new Error(await response.text());
      const body = await response.json();
      sites = body.sites || [];
    } catch (e) {
      error = e.message;
    }
  }

  async function addSite() {
    message = '';
    try {
      const response = await fetch('/api/sites', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(siteForm)
      });
      if (!response.ok) throw new Error(await response.text());
      const body = await response.json();
      if (!body.success) throw new Error(body.error || 'site save failed');
      form.site = body.site.name;
      siteForm = { name: '', kind: 'dc', parent_id: '', metadata_json: '{}' };
      message = `Site ${body.site.name} is available for onboarding.`;
      await loadSites();
    } catch (e) {
      error = e.message;
    }
  }

  async function addCredential() {
    message = '';
    try {
      const response = await fetch('/api/credentials', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(credentialForm)
      });
      if (!response.ok) throw new Error(await response.text());
      const body = await response.json();
      if (!body.success) throw new Error(body.error || 'credential save failed');
      form.credential_alias = body.credential.alias;
      credentialForm = { alias: '', username: '', password: '' };
      message = `Credential alias ${body.credential.alias} is stored in the local vault.`;
      invalidateDiscovery();
      await loadCredentials();
    } catch (e) {
      error = e.message;
    }
  }

  async function discoverDevice() {
    discovering = true;
    error = '';
    message = '';
    discovery = null;
    selectedProfileName = '';
    selectedPathIds = [];
    try {
      const response = await fetch('/api/onboarding/discover', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          address: form.address,
          credential_alias: form.credential_alias,
          username_env: form.username_env,
          password_env: form.password_env,
          ca_cert_path: form.ca_cert,
          tls_domain: form.tls_domain,
          role_hint: form.role
        })
      });
      if (!response.ok) throw new Error(await response.text());
      discovery = await response.json();
      form.vendor = discovery.vendor_detected || form.vendor;
      const firstProfile = discovery.recommended_profiles?.[0];
      if (firstProfile) selectProfile(firstProfile.profile_name);
      message = `Discovery succeeded: ${discovery.vendor_detected || 'openconfig'} with ${discovery.models_advertised.length} advertised models.`;
    } catch (e) {
      error = e.message;
    } finally {
      discovering = false;
    }
  }

  async function saveDevice() {
    const paths = selectedPaths();
    if (!paths.length) {
      error = 'Select at least one subscription path before saving.';
      step = 3;
      return;
    }

    saving = true;
    error = '';
    message = '';
    try {
      const response = await fetch('/api/onboarding/devices/with_paths', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          ...form,
          selected_paths: paths.map((path) => ({
            path: path.path,
            origin: path.origin || '',
            mode: path.mode,
            sample_interval_ns: path.sample_interval_ns || 0,
            rationale: path.rationale || '',
            optional: !!path.optional
          }))
        })
      });
      if (!response.ok) throw new Error(await response.text());
      const body = await response.json();
      if (!body.success) throw new Error(body.error || 'device save failed');
      message = `Device ${body.device.address} is managed with ${paths.length} selected subscription paths.`;
      workspace = 'devices';
      await loadDevices();
    } catch (e) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  async function removeDevice(address) {
    if (!confirm(`Remove ${address} from the runtime registry?`)) return;
    try {
      const response = await fetch('/api/onboarding/devices/remove', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ address })
      });
      if (!response.ok) throw new Error(await response.text());
      const body = await response.json();
      if (!body.success) throw new Error(body.error || 'device remove failed');
      message = `Removed ${address}; subscriber cancellation is in progress.`;
      await loadDevices();
    } catch (e) {
      error = e.message;
    }
  }

  function editDevice(device) {
    form = {
      address: device.address,
      hostname: device.hostname,
      vendor: device.vendor,
      role: device.role || 'leaf',
      site: device.site || 'lab',
      username_env: device.username_env,
      password_env: device.password_env,
      credential_alias: device.credential_alias,
      tls_domain: device.tls_domain,
      ca_cert: device.ca_cert
    };
    discovery = null;
    selectedProfileName = '';
    selectedPathIds = [];
    step = 1;
    workspace = 'wizard';
    message = `Editing ${device.address}. Run discovery again before saving path changes.`;
  }

  function resetForm() {
    form = emptyForm();
    discovery = null;
    selectedProfileName = '';
    selectedPathIds = [];
    step = 1;
    message = '';
    error = '';
  }

  function invalidateDiscovery() {
    if (discovery) {
      discovery = null;
      selectedProfileName = '';
      selectedPathIds = [];
      if (step > 1) step = 1;
      message = 'Discovery was cleared because the connection inputs changed.';
    }
  }

  function selectProfile(profileName) {
    const profile = profileByName(profileName);
    if (!profile) return;
    selectedProfileName = profile.profile_name;
    selectedPathIds = profile.paths.map(pathId);
  }

  function togglePath(path) {
    if (!path.optional) return;
    const id = pathId(path);
    if (selectedPathIds.includes(id)) {
      selectedPathIds = selectedPathIds.filter((value) => value !== id);
    } else {
      selectedPathIds = [...selectedPathIds, id];
    }
  }

  function profileByName(profileName) {
    return discovery?.recommended_profiles?.find((profile) => profile.profile_name === profileName);
  }

  function currentProfile() {
    return profileByName(selectedProfileName) || discovery?.recommended_profiles?.[0] || null;
  }

  function selectedPaths() {
    const profile = currentProfile();
    if (!profile) return [];
    return profile.paths.filter((path) => selectedPathIds.includes(pathId(path)) || !path.optional);
  }

  function pathId(path) {
    return `${path.origin || ''}|${path.mode}|${path.sample_interval_ns || 0}|${path.path}`;
  }

  function nextStep() {
    error = '';
    if (step === 1 && !form.address.trim()) {
      error = 'gNMI address is required before discovery.';
      return;
    }
    if (step === 2 && !discovery) {
      error = 'Run discovery before choosing a path profile.';
      return;
    }
    if (step === 3 && !selectedPaths().length) {
      error = 'Select at least one subscription path before confirming.';
      return;
    }
    step = Math.min(4, step + 1);
  }

  function previousStep() {
    error = '';
    step = Math.max(1, step - 1);
  }

  function statusClass(status) {
    if (status === 'observed') return 'healthy';
    if (status === 'pending') return 'info';
    return 'critical';
  }

  onMount(() => {
    loadDevices();
    loadCredentials();
    loadSites();
    const interval = setInterval(loadDevices, 10000);
    return () => clearInterval(interval);
  });
</script>

<div class="view onboarding">
  <section class="workspace-header">
    <div>
      <p class="eyebrow">Runtime onboarding</p>
      <h2>Bring a device online like a flight check.</h2>
      <p class="muted">Pick server-side credentials, prove Capabilities, choose the exact subscription plan, then let Bonsai start the subscriber.</p>
    </div>
    <div class="workspace-switcher" aria-label="Onboarding workspace">
      <button class:active={workspace === 'wizard'} onclick={() => workspace = 'wizard'}>Wizard</button>
      <button class:active={workspace === 'devices'} onclick={() => workspace = 'devices'}>Device list</button>
      <button class="ghost" onclick={loadDevices}>Refresh</button>
    </div>
  </section>

  {#if error}
    <div class="notice error">{error}</div>
  {/if}
  {#if message}
    <div class="notice success">{message}</div>
  {/if}

  {#if workspace === 'wizard'}
    <section class="wizard-shell">
      <aside class="wizard-rail">
        {#each STEPS as item}
          <button class:active={step === item.id} class:complete={step > item.id} onclick={() => step = item.id}>
            <span>{item.id}</span>
            {item.label}
          </button>
        {/each}
      </aside>

      <div class="wizard-panel">
        {#if step === 1}
          <div class="panel-heading">
            <p class="eyebrow">Step 1</p>
            <h3>Address and credentials</h3>
            <p class="muted">Vault aliases are preferred. Env vars remain available for lab compatibility, but secrets never enter the registry JSON.</p>
          </div>

          <div class="form-grid">
            <div class="form-row span-2">
              <label for="onboard-address">gNMI address</label>
              <input id="onboard-address" bind:value={form.address} oninput={invalidateDiscovery} placeholder="172.100.102.12:57400" required />
            </div>
            <div class="form-row">
              <label for="onboard-hostname">Hostname</label>
              <input id="onboard-hostname" bind:value={form.hostname} placeholder="srl-leaf1" />
            </div>
            <div class="form-row">
              <label for="onboard-role">Role</label>
              <select id="onboard-role" bind:value={form.role} onchange={invalidateDiscovery}>
                <option value="leaf">leaf</option>
                <option value="spine">spine</option>
                <option value="pe">pe</option>
                <option value="p">p</option>
                <option value="rr">rr</option>
              </select>
            </div>
            <div class="form-row">
              <label for="onboard-credential-alias">Credential alias</label>
              <select id="onboard-credential-alias" bind:value={form.credential_alias} onchange={invalidateDiscovery}>
                <option value="">No vault alias</option>
                {#each credentials as credential}
                  <option value={credential.alias}>{credential.alias}</option>
                {/each}
              </select>
            </div>
            <div class="form-row">
              <label for="onboard-site">Site</label>
              <select id="onboard-site" bind:value={form.site}>
                <option value="">No site</option>
                {#each sites as site}
                  <option value={site.name}>{site.name} ({site.kind || 'unknown'})</option>
                {/each}
                {#if form.site && !sites.some((site) => site.name === form.site)}
                  <option value={form.site}>{form.site}</option>
                {/if}
              </select>
            </div>
            <div class="form-row">
              <label for="onboard-username-env">Username env var</label>
              <input id="onboard-username-env" bind:value={form.username_env} oninput={invalidateDiscovery} placeholder="BONSAI_GNMI_USER" />
            </div>
            <div class="form-row">
              <label for="onboard-password-env">Password env var</label>
              <input id="onboard-password-env" bind:value={form.password_env} oninput={invalidateDiscovery} placeholder="BONSAI_GNMI_PASS" />
            </div>
            <div class="form-row">
              <label for="onboard-tls-domain">TLS domain</label>
              <input id="onboard-tls-domain" bind:value={form.tls_domain} oninput={invalidateDiscovery} placeholder="clab-bonsai-p4-srl-leaf1" />
            </div>
            <div class="form-row">
              <label for="onboard-ca-cert">CA cert path</label>
              <input id="onboard-ca-cert" bind:value={form.ca_cert} oninput={invalidateDiscovery} placeholder="lab/fast-iteration/p4-ca.pem" />
            </div>
          </div>

          <div class="sidecar-grid">
            <section class="tool-card">
              <h4>Credential vault</h4>
              <p class="muted">{vaultUnlocked ? 'Vault unlocked. Devices store aliases only.' : 'Vault locked. Start Bonsai with BONSAI_VAULT_PASSPHRASE to add or use aliases.'}</p>
              <form class="compact-form" onsubmit={(event) => { event.preventDefault(); addCredential(); }}>
                <input bind:value={credentialForm.alias} placeholder="srl-lab-admin" disabled={!vaultUnlocked} />
                <input bind:value={credentialForm.username} placeholder="username" autocomplete="username" disabled={!vaultUnlocked} />
                <input bind:value={credentialForm.password} placeholder="password" type="password" autocomplete="new-password" disabled={!vaultUnlocked} />
                <button type="submit" disabled={!vaultUnlocked || !credentialForm.alias || !credentialForm.username || !credentialForm.password}>Store alias</button>
              </form>
            </section>

            <section class="tool-card">
              <h4>Sites</h4>
              <p class="muted">Sites become graph nodes; saved devices get a LOCATED_AT edge.</p>
              <form class="compact-form" onsubmit={(event) => { event.preventDefault(); addSite(); }}>
                <input bind:value={siteForm.name} placeholder="lab-london" />
                <select bind:value={siteForm.kind}>
                  <option value="region">region</option>
                  <option value="country">country</option>
                  <option value="city">city</option>
                  <option value="dc">dc</option>
                  <option value="rack">rack</option>
                  <option value="unknown">unknown</option>
                </select>
                <select bind:value={siteForm.parent_id}>
                  <option value="">No parent</option>
                  {#each sites as site}
                    <option value={site.id}>{site.name}</option>
                  {/each}
                </select>
                <button type="submit" disabled={!siteForm.name}>Add site</button>
              </form>
            </section>
          </div>
        {:else if step === 2}
          <div class="panel-heading">
            <p class="eyebrow">Step 2</p>
            <h3>Discovery report</h3>
            <p class="muted">Bonsai calls gNMI Capabilities with the chosen credential alias or env vars, then ranks path profiles for this role.</p>
          </div>

          <div class="actions">
            <button type="button" onclick={discoverDevice} disabled={discovering || !form.address}>
              {discovering ? 'Discovering...' : 'Run discovery'}
            </button>
            {#if discovery}
              <button type="button" class="ghost" onclick={discoverDevice}>Refresh discovery</button>
            {/if}
          </div>

          {#if discovery}
            <div class="report-grid">
              <div class="metric"><span>Vendor</span><strong>{discovery.vendor_detected || 'unknown'}</strong></div>
              <div class="metric"><span>Encoding</span><strong>{discovery.gnmi_encoding || 'unknown'}</strong></div>
              <div class="metric"><span>Models</span><strong>{discovery.models_advertised.length}</strong></div>
              <div class="metric"><span>Profiles</span><strong>{discovery.recommended_profiles.length}</strong></div>
            </div>
            <details class="model-list" open>
              <summary>Advertised models</summary>
              {#each discovery.models_advertised as model}
                <code>{model}</code>
              {/each}
            </details>
            {#if discovery.warnings.length}
              <div class="warning-stack">
                {#each discovery.warnings as warning}
                  <p class="warning">{warning}</p>
                {/each}
              </div>
            {/if}
          {:else}
            <p class="empty">No report yet. Run discovery to unlock path profile selection.</p>
          {/if}
        {:else if step === 3}
          <div class="panel-heading">
            <p class="eyebrow">Step 3</p>
            <h3>Profile and path selection</h3>
            <p class="muted">Required paths stay armed. Optional paths can be removed if the lab image advertises them but you do not want that stream yet.</p>
          </div>

          {#if discovery?.recommended_profiles?.length}
            <div class="profile-grid">
              {#each discovery.recommended_profiles as profile}
                <button class="profile-card" class:active={currentProfile()?.profile_name === profile.profile_name} onclick={() => selectProfile(profile.profile_name)}>
                  <strong>{profile.profile_name}</strong>
                  <span>{profile.paths.length} paths - {Math.round(profile.confidence * 100)}% confidence</span>
                  <p>{profile.rationale}</p>
                </button>
              {/each}
            </div>

            {#if currentProfile()}
              <div class="path-checklist">
                {#each currentProfile().paths as path}
                  <label class:optional={path.optional}>
                    <input
                      type="checkbox"
                      checked={selectedPathIds.includes(pathId(path)) || !path.optional}
                      disabled={!path.optional}
                      onchange={() => togglePath(path)}
                    />
                    <span>
                      <strong>{path.mode}{path.optional ? ' optional' : ' required'}</strong>
                      <code>{path.origin ? `${path.origin}:` : ''}{path.path}</code>
                      <small>{path.sample_interval_ns ? `${path.sample_interval_ns} ns sample` : 'on-change stream'} - {path.rationale}</small>
                    </span>
                  </label>
                {/each}
              </div>
            {/if}
          {:else}
            <p class="empty">Run discovery first; path profiles are produced from the Capabilities response.</p>
          {/if}
        {:else}
          <div class="panel-heading">
            <p class="eyebrow">Step 4</p>
            <h3>Confirm subscriber plan</h3>
            <p class="muted">Saving writes the registry entry and selected paths, then the runtime subscriber manager starts or restarts the device.</p>
          </div>

          <div class="confirm-card">
            <div><span>Target</span><strong>{form.hostname || form.address}</strong><small>{form.address}</small></div>
            <div><span>Credential</span><strong>{form.credential_alias || 'env vars / lab config'}</strong><small>{form.username_env || 'no username env'} / {form.password_env || 'no password env'}</small></div>
            <div><span>Profile</span><strong>{currentProfile()?.profile_name || 'none'}</strong><small>{selectedPaths().length} selected paths</small></div>
            <div><span>Expected telemetry</span><strong>pending -> observed</strong><small>SubscriptionStatus rows appear first, then flip after matching updates arrive.</small></div>
          </div>

          <div class="selected-path-summary">
            {#each selectedPaths() as path}
              <code>{path.origin ? `${path.origin}:` : ''}{path.path}</code>
            {/each}
          </div>
        {/if}

        <div class="wizard-actions">
          <button type="button" class="ghost" onclick={previousStep} disabled={step === 1}>Back</button>
          <button type="button" class="ghost" onclick={resetForm}>Clear</button>
          {#if step < 4}
            <button type="button" onclick={nextStep}>Next</button>
          {:else}
            <button type="button" onclick={saveDevice} disabled={saving || !selectedPaths().length}>
              {saving ? 'Saving...' : 'Save and subscribe'}
            </button>
          {/if}
        </div>
      </div>
    </section>
  {:else}
    <section class="managed-section separate-workspace">
      <div class="section-title">
        <h3>Managed devices</h3>
        <span>{devices.length} active registry entries</span>
      </div>

      {#if loading}
        <p class="empty">Loading managed devices...</p>
      {:else if !devices.length}
        <p class="empty">No managed devices yet. Add one in the wizard to start the subscriber lifecycle.</p>
      {:else}
        <div class="device-list">
          {#each devices as device}
            <article class="managed-device">
              <header>
                <div>
                  <h4>{device.hostname || device.address}</h4>
                  <p>{device.address} - {device.vendor || 'vendor pending'} - {device.role || 'role unset'} - {device.credential_alias || 'env credentials'}</p>
                </div>
                <div class="device-actions">
                  <button class="ghost" onclick={() => editDevice(device)}>Edit</button>
                  <button class="danger" onclick={() => removeDevice(device.address)}>Remove</button>
                </div>
              </header>

              {#if device.selected_paths?.length}
                <div class="armed-paths">
                  <span>{device.selected_paths.length} armed paths</span>
                  {#each device.selected_paths.slice(0, 4) as path}
                    <code>{path.origin ? `${path.origin}:` : ''}{path.path}</code>
                  {/each}
                </div>
              {/if}

              {#if device.subscription_statuses.length}
                <div class="status-list">
                  {#each device.subscription_statuses as status}
                    <div class="status-row">
                      <span class="badge {statusClass(status.status)}">{status.status}</span>
                      <code>{status.path}</code>
                      <small>{status.mode}{status.origin ? ` - ${status.origin}` : ''}</small>
                    </div>
                  {/each}
                </div>
              {:else}
                <p class="muted">No subscription status yet. After save, expect pending paths first, then observed once telemetry arrives.</p>
              {/if}
            </article>
          {/each}
        </div>
      {/if}
    </section>
  {/if}
</div>
