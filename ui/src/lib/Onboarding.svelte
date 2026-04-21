<script>
  import { onMount } from 'svelte';

  let loading = $state(true);
  let saving = $state(false);
  let discovering = $state(false);
  let error = $state('');
  let message = $state('');
  let devices = $state([]);
  let discovery = $state(null);

  let form = $state({
    address: '',
    hostname: '',
    vendor: '',
    role: 'leaf',
    site: 'lab',
    username_env: '',
    password_env: '',
    tls_domain: '',
    ca_cert: ''
  });

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

  async function discoverDevice() {
    discovering = true;
    message = '';
    discovery = null;
    try {
      const response = await fetch('/api/onboarding/discover', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          address: form.address,
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
      message = `Discovery succeeded: ${discovery.vendor_detected || 'openconfig'} with ${discovery.models_advertised.length} advertised models.`;
    } catch (e) {
      error = e.message;
    } finally {
      discovering = false;
    }
  }

  async function saveDevice() {
    saving = true;
    message = '';
    try {
      const response = await fetch('/api/onboarding/devices', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(form)
      });
      if (!response.ok) throw new Error(await response.text());
      const body = await response.json();
      if (!body.success) throw new Error(body.error || 'device save failed');
      message = `Device ${body.device.address} is managed. Subscriber lifecycle will start or restart automatically.`;
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
      tls_domain: device.tls_domain,
      ca_cert: device.ca_cert
    };
    discovery = null;
    message = `Editing ${device.address}. Saving will restart its subscriber if values changed.`;
  }

  function resetForm() {
    form = {
      address: '',
      hostname: '',
      vendor: '',
      role: 'leaf',
      site: 'lab',
      username_env: '',
      password_env: '',
      tls_domain: '',
      ca_cert: ''
    };
    discovery = null;
    message = '';
  }

  function statusClass(status) {
    if (status === 'observed') return 'healthy';
    if (status === 'pending') return 'info';
    return 'critical';
  }

  onMount(() => {
    loadDevices();
    const interval = setInterval(loadDevices, 10000);
    return () => clearInterval(interval);
  });
</script>

<div class="view onboarding">
  <section class="workspace-header">
    <div>
      <p class="eyebrow">Runtime onboarding</p>
      <h2>Discover, add, and verify devices without restarting Bonsai.</h2>
      <p class="muted">Credentials stay outside source code. Enter environment variable names, run discovery, then save the device into the runtime registry.</p>
    </div>
    <button class="ghost" onclick={loadDevices}>Refresh status</button>
  </section>

  {#if error}
    <div class="notice error">{error}</div>
  {/if}
  {#if message}
    <div class="notice success">{message}</div>
  {/if}

  <section class="onboarding-grid">
    <form class="onboarding-form" onsubmit={(event) => { event.preventDefault(); saveDevice(); }}>
      <div class="form-row span-2">
        <label for="onboard-address">gNMI address</label>
        <input id="onboard-address" bind:value={form.address} placeholder="172.100.102.12:57400" required />
      </div>

      <div class="form-row">
        <label for="onboard-hostname">Hostname</label>
        <input id="onboard-hostname" bind:value={form.hostname} placeholder="srl-leaf1" />
      </div>

      <div class="form-row">
        <label for="onboard-role">Role</label>
        <select id="onboard-role" bind:value={form.role}>
          <option value="leaf">leaf</option>
          <option value="spine">spine</option>
          <option value="pe">pe</option>
          <option value="p">p</option>
          <option value="rr">rr</option>
        </select>
      </div>

      <div class="form-row">
        <label for="onboard-username-env">Username env var</label>
        <input id="onboard-username-env" bind:value={form.username_env} placeholder="BONSAI_GNMI_USER" />
      </div>

      <div class="form-row">
        <label for="onboard-password-env">Password env var</label>
        <input id="onboard-password-env" bind:value={form.password_env} placeholder="BONSAI_GNMI_PASS" />
      </div>

      <div class="form-row">
        <label for="onboard-tls-domain">TLS domain</label>
        <input id="onboard-tls-domain" bind:value={form.tls_domain} placeholder="clab-bonsai-p4-srl-leaf1" />
      </div>

      <div class="form-row">
        <label for="onboard-ca-cert">CA cert path</label>
        <input id="onboard-ca-cert" bind:value={form.ca_cert} placeholder="lab/fast-iteration/p4-ca.pem" />
      </div>

      <div class="form-row">
        <label for="onboard-vendor">Vendor label</label>
        <input id="onboard-vendor" bind:value={form.vendor} placeholder="auto-filled after discovery" />
      </div>

      <div class="form-row">
        <label for="onboard-site">Site</label>
        <input id="onboard-site" bind:value={form.site} placeholder="lab" />
      </div>

      <div class="actions span-2">
        <button type="button" class="ghost" onclick={discoverDevice} disabled={discovering || !form.address}>
          {discovering ? 'Discovering...' : 'Discover'}
        </button>
        <button type="submit" disabled={saving || !form.address}>
          {saving ? 'Saving...' : 'Save and subscribe'}
        </button>
        <button type="button" class="ghost" onclick={resetForm}>Clear</button>
      </div>
    </form>

    <aside class="discovery-panel">
      <h3>Discovery report</h3>
      {#if discovery}
        <div class="report-line">
          <span>Vendor</span>
          <strong>{discovery.vendor_detected}</strong>
        </div>
        <div class="report-line">
          <span>Encoding</span>
          <strong>{discovery.gnmi_encoding}</strong>
        </div>
        <div class="report-line">
          <span>Models</span>
          <strong>{discovery.models_advertised.length}</strong>
        </div>
        {#if discovery.recommended_profiles.length}
          <h4>Recommended profiles</h4>
          {#each discovery.recommended_profiles as profile}
            <div class="profile">
              <strong>{profile.profile_name}</strong>
              <span>{profile.paths.length} paths · confidence {Math.round(profile.confidence * 100)}%</span>
              <p>{profile.rationale}</p>
            </div>
          {/each}
        {/if}
        {#if discovery.warnings.length}
          <h4>Warnings</h4>
          {#each discovery.warnings as warning}
            <p class="warning">{warning}</p>
          {/each}
        {/if}
      {:else}
        <p class="muted">Run discovery to confirm Capabilities and path profile recommendations before saving.</p>
      {/if}
    </aside>
  </section>

  <section class="managed-section">
    <div class="section-title">
      <h3>Managed devices</h3>
      <span>{devices.length} active registry entries</span>
    </div>

    {#if loading}
      <p class="empty">Loading managed devices...</p>
    {:else if !devices.length}
      <p class="empty">No managed devices yet. Add one above to start the subscriber lifecycle.</p>
    {:else}
      <div class="device-list">
        {#each devices as device}
          <article class="managed-device">
            <header>
              <div>
                <h4>{device.hostname || device.address}</h4>
                <p>{device.address} · {device.vendor || 'vendor pending'} · {device.role || 'role unset'}</p>
              </div>
              <div class="device-actions">
                <button class="ghost" onclick={() => editDevice(device)}>Edit</button>
                <button class="danger" onclick={() => removeDevice(device.address)}>Remove</button>
              </div>
            </header>

            {#if device.subscription_statuses.length}
              <div class="status-list">
                {#each device.subscription_statuses as status}
                  <div class="status-row">
                    <span class="badge {statusClass(status.status)}">{status.status}</span>
                    <code>{status.path}</code>
                    <small>{status.mode}{status.origin ? ` · ${status.origin}` : ''}</small>
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
</div>
