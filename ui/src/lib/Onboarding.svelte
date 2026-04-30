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
  let environments = $state([]);
  let selectedEnvironmentId = $state('');
  let vaultUnlocked = $state(false);
  let discovery = $state(null);
  let selectedProfileName = $state('');
  let selectedPathIds = $state([]);
  let editingDeviceAddress = $state('');
  let editingSavedPaths = $state([]);
  let selectedDeviceAddresses = $state([]);
  let events = null;
  let refreshTimer = null;

  // ── Custom path customisation (T2-6) ─────────────────────────────────────
  let extraPaths = $state([]);        // manually added or browsed-from-catalogue paths
  let allProfiles = $state([]);       // full catalogue, loaded for browsing
  let showCatalogueBrowser = $state(false);
  let showManualPathForm = $state(false);
  let showSaveCustomModal = $state(false);
  let browsedProfile = $state(null);  // profile being inspected in the catalogue browser
  let savingCustom = $state(false);
  let manualPath = $state({ path: '', origin: '', mode: 'ON_CHANGE', sample_interval_ns: 0, rationale: '' });
  let customProfileName = $state('');

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
      enabled: true,
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
      selectedDeviceAddresses = selectedDeviceAddresses.filter((address) =>
        devices.some((device) => device.address === address)
      );
      error = '';
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function scheduleDeviceRefresh() {
    if (document.hidden) return;
    if (refreshTimer) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => {
      refreshTimer = null;
      loadDevices();
    }, 250);
  }

  function shouldRefreshForEvent(ev) {
    return ev.event_type?.startsWith('registry_') || ev.event_type === 'subscription_status_change';
  }

  function connectEvents() {
    if (events || document.hidden) return;
    events = new EventSource('/api/events');
    events.onmessage = (messageEvent) => {
      try {
        const ev = JSON.parse(messageEvent.data);
        if (shouldRefreshForEvent(ev)) scheduleDeviceRefresh();
      } catch {}
    };
    events.onerror = () => {
      /* Browser-managed SSE reconnect keeps the onboarding view event-driven. */
    };
  }

  function disconnectEvents() {
    if (!events) return;
    events.close();
    events = null;
  }

  function handleVisibilityChange() {
    if (document.hidden) {
      disconnectEvents();
      if (refreshTimer) {
        clearTimeout(refreshTimer);
        refreshTimer = null;
      }
      return;
    }
    loadDevices();
    connectEvents();
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

  async function loadEnvironments() {
    try {
      const response = await fetch('/api/environments');
      if (!response.ok) return;
      const body = await response.json();
      environments = body.environments || [];
    } catch (_) {}
  }

  const ROLES_BY_ARCHETYPE = {
    data_center:       ['leaf', 'spine', 'superspine', 'border', 'edge'],
    service_provider:  ['pe', 'p', 'rr', 'ce-facing', 'peering'],
    campus_wired:      ['access', 'distribution', 'core', 'border'],
    campus_wireless:   ['ap', 'wlc', 'edge-wlc'],
    home_lab:          ['leaf', 'spine', 'pe', 'p', 'rr', 'router', 'switch'],
  };

  const ALL_ROLES = ['leaf', 'spine', 'superspine', 'border', 'edge', 'pe', 'p', 'rr', 'ce-facing', 'peering', 'access', 'distribution', 'core', 'ap', 'wlc', 'edge-wlc', 'router', 'switch'];

  let activeRoles = $derived(() => {
    if (!selectedEnvironmentId) return ALL_ROLES;
    const env = environments.find(e => e.id === selectedEnvironmentId);
    return ROLES_BY_ARCHETYPE[env?.archetype] ?? ALL_ROLES;
  });

  let filteredSites = $derived(() => {
    if (!selectedEnvironmentId) return sites;
    return sites.filter(s => s.environment_id === selectedEnvironmentId);
  });

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
      const matchedCount = applyInitialPathSelection();
      const editNote = editingDeviceAddress
        ? ` ${matchedCount} previously saved path${matchedCount === 1 ? '' : 's'} matched current recommendations.`
        : '';
      message = `Discovery succeeded: ${discovery.vendor_detected || 'openconfig'} with ${discovery.models_advertised.length} advertised models.${editNote}`;
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
          enabled: form.enabled,
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
      message = editingDeviceAddress
        ? `Device ${body.device.address} was updated with ${paths.length} selected subscription paths.`
        : `Device ${body.device.address} is managed with ${paths.length} selected subscription paths.`;
      editingDeviceAddress = '';
      editingSavedPaths = [];
      workspace = 'devices';
      await loadDevices();
    } catch (e) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  async function removeDevice(address) {
    let impact = null;
    try {
      const response = await fetch('/api/onboarding/devices/remove-impact', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ address })
      });
      if (response.ok) impact = await response.json();
    } catch (_) {
      impact = null;
    }

    const impactText = impact
      ? `\n\nSubscriptions: ${impact.subscription_total} total (${impact.subscription_observed} observed, ${impact.subscription_pending} pending)\nRemediation trust marks: ${impact.trust_marks_total} linked, ${impact.trust_marks_active} active/trusted`
      : '';
    if (!confirm(`Remove ${address} from the runtime registry?${impactText}`)) return;
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
      selectedDeviceAddresses = selectedDeviceAddresses.filter((value) => value !== address);
      await loadDevices();
    } catch (e) {
      error = e.message;
    }
  }

  function editDevice(device) {
    editingDeviceAddress = device.address;
    editingSavedPaths = device.selected_paths || [];
    form = {
      address: device.address,
      hostname: device.hostname,
      vendor: device.vendor,
      role: device.role || 'leaf',
      site: device.site || 'lab',
      enabled: device.enabled,
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
    error = '';
    message = `Editing ${device.address}. The wizard is pre-populated; run discovery to revalidate its saved path plan before saving.`;
  }

  function resetForm() {
    form = emptyForm();
    discovery = null;
    selectedProfileName = '';
    selectedPathIds = [];
    editingDeviceAddress = '';
    editingSavedPaths = [];
    selectedDeviceAddresses = [];
    step = 1;
    message = '';
    error = '';
  }

  function invalidateDiscovery() {
    if (discovery) {
      discovery = null;
      selectedProfileName = '';
      selectedPathIds = [];
      extraPaths = [];
      if (step > 1) step = 1;
      message = 'Discovery was cleared because the connection inputs changed.';
    }
  }

  function selectProfile(profileName) {
    const profile = profileByName(profileName);
    if (!profile) return;
    armProfile(profile);
  }

  function applyInitialPathSelection() {
    const profiles = discovery?.recommended_profiles || [];
    if (!profiles.length) return 0;
    if (!editingSavedPaths.length) {
      armProfile(profiles[0]);
      return 0;
    }

    const savedIds = new Set(editingSavedPaths.map(pathId));
    const ranked = profiles
      .map((profile) => ({
        profile,
        matches: profile.paths.filter((path) => savedIds.has(pathId(path))).length
      }))
      .sort((a, b) => b.matches - a.matches);

    const best = ranked[0];
    armProfile(best.profile, editingSavedPaths);
    return best.matches;
  }

  function armProfile(profile, preferredPaths = []) {
    const preferredIds = new Set(preferredPaths.map(pathId));
    selectedProfileName = profile.profile_name;
    selectedPathIds = [
      ...new Set(
        profile.paths
          .filter((path) => !path.optional || !preferredPaths.length || preferredIds.has(pathId(path)))
          .map(pathId)
      )
    ];
    extraPaths = [];
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

  async function bulkDeviceAction(action) {
    if (!selectedDeviceAddresses.length) {
      error = 'Select at least one device first.';
      return;
    }
    const label = action === 'stop' ? 'stop' : action === 'start' ? 'start' : 'restart';
    if (!confirm(`${label} ${selectedDeviceAddresses.length} selected device(s)?`)) return;

    error = '';
    message = '';
    try {
      const response = await fetch('/api/onboarding/devices/bulk', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ addresses: selectedDeviceAddresses, action })
      });
      if (!response.ok) throw new Error(await response.text());
      const body = await response.json();
      if (!body.success) throw new Error(body.error || `bulk ${label} failed`);
      message = `${label} requested for ${body.devices.length} device(s).`;
      selectedDeviceAddresses = [];
      await loadDevices();
    } catch (e) {
      error = e.message;
    }
  }

  function toggleDeviceSelection(address) {
    if (selectedDeviceAddresses.includes(address)) {
      selectedDeviceAddresses = selectedDeviceAddresses.filter((value) => value !== address);
    } else {
      selectedDeviceAddresses = [...selectedDeviceAddresses, address];
    }
  }

  function toggleAllDevices() {
    if (selectedDeviceAddresses.length === devices.length) {
      selectedDeviceAddresses = [];
    } else {
      selectedDeviceAddresses = devices.map((device) => device.address);
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
    const profilePaths = profile
      ? profile.paths.filter((path) => selectedPathIds.includes(pathId(path)) || !path.optional)
      : [];
    // De-duplicate: skip extra paths already in the profile selection
    const profileIds = new Set(profilePaths.map(pathId));
    const uniqueExtras = extraPaths.filter((p) => !profileIds.has(pathId(p)));
    return [...profilePaths, ...uniqueExtras];
  }

  function pathId(path) {
    return `${path.origin || ''}|${path.mode}|${path.sample_interval_ns || 0}|${path.path}`;
  }

  async function loadAllProfiles() {
    try {
      const res = await fetch('/api/profiles');
      if (!res.ok) return;
      const body = await res.json();
      allProfiles = body.profiles || [];
    } catch (_) {}
  }

  function profilesForBrowser() {
    // Exclude the currently selected profile — operator is adding from other profiles
    return allProfiles.filter((p) => p.name !== selectedProfileName);
  }

  function pathsForBrowsedProfile() {
    if (!browsedProfile) return [];
    // We only have path_count in the index; need full paths. Use discovery recommended_profiles
    // if browsedProfile matches, else we show paths from allProfiles detail (not available without
    // an extra API call). Simplification: use discovery recommended_profiles for the selected
    // device; for catalogue browser show paths from recommended_profiles if present.
    const inDiscovery = discovery?.recommended_profiles?.find((p) => p.profile_name === browsedProfile.name);
    if (inDiscovery) return inDiscovery.paths;
    return [];
  }

  async function fetchProfilePaths(profileName) {
    // Try to get full paths from discovery recommended profiles first (already in memory).
    const inDiscovery = discovery?.recommended_profiles?.find((p) => p.profile_name === profileName);
    if (inDiscovery) return inDiscovery.paths;
    // Fallback: fetch via a discover call is not appropriate here; signal that paths aren't available.
    return null;
  }

  async function openCatalogueBrowser() {
    await loadAllProfiles();
    browsedProfile = null;
    showCatalogueBrowser = true;
  }

  async function selectBrowsedProfile(profile) {
    const paths = await fetchProfilePaths(profile.name);
    browsedProfile = { ...profile, loadedPaths: paths };
  }

  function addExtraPath(path) {
    const id = pathId(path);
    const alreadyExtra = extraPaths.some((p) => pathId(p) === id);
    const inProfile = currentProfile()?.paths.some((p) => pathId(p) === id);
    if (!alreadyExtra && !inProfile) {
      extraPaths = [...extraPaths, { ...path, _extra: true }];
    }
  }

  function removeExtraPath(path) {
    const id = pathId(path);
    extraPaths = extraPaths.filter((p) => pathId(p) !== id);
  }

  function addManualPath() {
    if (!manualPath.path.trim()) return;
    addExtraPath({
      path: manualPath.path.trim(),
      origin: manualPath.origin.trim(),
      mode: manualPath.mode || 'ON_CHANGE',
      sample_interval_ns: Number(manualPath.sample_interval_ns) || 0,
      rationale: manualPath.rationale.trim() || 'Manually added',
      optional: true,
    });
    manualPath = { path: '', origin: '', mode: 'ON_CHANGE', sample_interval_ns: 0, rationale: '' };
    showManualPathForm = false;
  }

  async function saveAsCustomProfile() {
    if (!customProfileName.trim()) return;
    savingCustom = true;
    error = '';
    try {
      const paths = selectedPaths().map((p) => ({
        path: p.path,
        origin: p.origin || '',
        mode: p.mode,
        sample_interval_ns: p.sample_interval_ns || 0,
        rationale: p.rationale || '',
        optional: !!p.optional,
        vendor_only: p.vendor_only || [],
      }));
      const env = environments.find((e) => e.id === selectedEnvironmentId);
      const res = await fetch('/api/profiles/save-custom', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          name: customProfileName.trim(),
          description: `Custom profile based on ${selectedProfileName || 'manual selection'}`,
          rationale: `Saved from device onboarding wizard for ${form.address || 'unknown device'}`,
          environment: env ? [env.archetype] : [],
          vendor_scope: [],
          roles: form.role ? [form.role] : [],
          paths,
        })
      });
      const body = await res.json();
      if (!body.success) throw new Error(body.error || 'save failed');
      message = `Custom profile "${customProfileName.trim()}" saved to catalogue.`;
      customProfileName = '';
      showSaveCustomModal = false;
    } catch (e) {
      error = e.message;
    } finally {
      savingCustom = false;
    }
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
    loadEnvironments();
    connectEvents();
    document.addEventListener('visibilitychange', handleVisibilityChange);
    return () => {
      document.removeEventListener('visibilitychange', handleVisibilityChange);
      disconnectEvents();
      if (refreshTimer) clearTimeout(refreshTimer);
    };
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
        {#if editingDeviceAddress}
          <div class="edit-banner">
            <div>
              <span>Editing existing device</span>
              <strong>{editingDeviceAddress}</strong>
            </div>
            <p>{editingSavedPaths.length ? `${editingSavedPaths.length} saved paths will be revalidated after discovery.` : 'No saved path plan exists yet; discovery will create one.'}</p>
          </div>
        {/if}

        {#if step === 1}
          <div class="panel-heading">
            <p class="eyebrow">Step 1</p>
            <h3>{editingDeviceAddress ? 'Review address and credentials' : 'Address and credentials'}</h3>
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
              <label for="onboard-environment">Environment</label>
              <select id="onboard-environment" bind:value={selectedEnvironmentId}>
                <option value="">Any / unassigned</option>
                {#each environments as env}
                  <option value={env.id}>{env.name}</option>
                {/each}
              </select>
            </div>
            <div class="form-row">
              <label for="onboard-role">Role</label>
              <select id="onboard-role" bind:value={form.role} onchange={invalidateDiscovery}>
                {#each activeRoles() as role}
                  <option value={role}>{role}</option>
                {/each}
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
                {#each filteredSites() as site}
                  <option value={site.name}>{site.name} ({site.kind || 'unknown'})</option>
                {/each}
                {#if form.site && !filteredSites().some((site) => site.name === form.site)}
                  <option value={form.site}>{form.site}</option>
                {/if}
              </select>
            </div>
            <label class="toggle-row">
              <input type="checkbox" bind:checked={form.enabled} />
              <span>
                <strong>Subscriber enabled</strong>
                <small>When off, the registry entry is saved but the runtime subscriber stays stopped.</small>
              </span>
            </label>
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
            {#if editingSavedPaths.length}
              <div class="saved-plan-note">
                <strong>Saved plan carried into wizard</strong>
                <span>{selectedPaths().length} selected paths are currently armed after matching the saved plan against discovery.</span>
              </div>
            {/if}
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

              {#if extraPaths.length}
                <div class="extra-paths-section">
                  <p class="eyebrow">Added paths</p>
                  <div class="path-checklist">
                    {#each extraPaths as path}
                      <label class="optional extra-path">
                        <input type="checkbox" checked disabled />
                        <span>
                          <strong>{path.mode} added</strong>
                          <code>{path.origin ? `${path.origin}:` : ''}{path.path}</code>
                          <small>{path.rationale}</small>
                        </span>
                        <button class="ghost small" onclick={() => removeExtraPath(path)} title="Remove this path">×</button>
                      </label>
                    {/each}
                  </div>
                </div>
              {/if}

              <div class="path-customise-toolbar">
                <button class="ghost" onclick={openCatalogueBrowser}>Browse catalogue</button>
                <button class="ghost" onclick={() => showManualPathForm = !showManualPathForm}>
                  {showManualPathForm ? 'Cancel' : '+ Manual path'}
                </button>
                {#if selectedPaths().length}
                  <button class="ghost" onclick={() => { customProfileName = ''; showSaveCustomModal = true; }}>Save as profile</button>
                {/if}
              </div>

              {#if showManualPathForm}
                <div class="manual-path-form">
                  <p class="eyebrow">Add a path manually</p>
                  <div class="form-row">
                    <label>
                      Path
                      <input type="text" bind:value={manualPath.path} placeholder="interfaces or Cisco-IOS-XR-..." />
                    </label>
                    <label>
                      Origin
                      <input type="text" bind:value={manualPath.origin} placeholder="openconfig (or blank)" />
                    </label>
                  </div>
                  <div class="form-row">
                    <label>
                      Mode
                      <select bind:value={manualPath.mode}>
                        <option>ON_CHANGE</option>
                        <option>SAMPLE</option>
                      </select>
                    </label>
                    {#if manualPath.mode === 'SAMPLE'}
                      <label>
                        Sample interval (ns)
                        <input type="number" bind:value={manualPath.sample_interval_ns} placeholder="10000000000" />
                      </label>
                    {/if}
                  </div>
                  <label>
                    Rationale
                    <input type="text" bind:value={manualPath.rationale} placeholder="Why this path?" />
                  </label>
                  <button onclick={addManualPath} disabled={!manualPath.path.trim()}>Add path</button>
                </div>
              {/if}
            {/if}

            {#if showCatalogueBrowser}
              <div class="catalogue-browser-overlay" role="dialog" aria-modal="true">
                <div class="catalogue-browser">
                  <div class="browser-header">
                    <h4>Browse catalogue profiles</h4>
                    <button class="ghost" onclick={() => { showCatalogueBrowser = false; browsedProfile = null; }}>Close</button>
                  </div>
                  <div class="browser-body">
                    <div class="browser-list">
                      {#each profilesForBrowser() as profile}
                        <button
                          class="browser-profile-item"
                          class:active={browsedProfile?.name === profile.name}
                          onclick={() => selectBrowsedProfile(profile)}
                        >
                          <strong>{profile.name}</strong>
                          <small>{profile.path_count} paths · {profile.environment?.join(', ') || 'any'}</small>
                        </button>
                      {/each}
                      {#if !profilesForBrowser().length}
                        <p class="empty">No other profiles available.</p>
                      {/if}
                    </div>
                    <div class="browser-paths">
                      {#if browsedProfile}
                        <p class="eyebrow">{browsedProfile.name}</p>
                        {#if browsedProfile.loadedPaths}
                          {#each browsedProfile.loadedPaths as path}
                            {@const alreadySelected = selectedPaths().some((p) => pathId(p) === pathId(path))}
                            <div class="browser-path-row" class:already-selected={alreadySelected}>
                              <span>
                                <code>{path.origin ? `${path.origin}:` : ''}{path.path}</code>
                                <small>{path.mode} — {path.rationale}</small>
                              </span>
                              <button
                                class="ghost small"
                                disabled={alreadySelected}
                                onclick={() => addExtraPath(path)}
                              >{alreadySelected ? 'Added' : '+ Add'}</button>
                            </div>
                          {/each}
                        {:else}
                          <p class="empty">Profile paths are only available for devices that included this profile in their discovery result. Run discovery against this device first.</p>
                        {/if}
                      {:else}
                        <p class="empty">Select a profile on the left to browse its paths.</p>
                      {/if}
                    </div>
                  </div>
                </div>
              </div>
            {/if}

            {#if showSaveCustomModal}
              <div class="catalogue-browser-overlay" role="dialog" aria-modal="true">
                <div class="save-custom-modal">
                  <h4>Save as custom profile</h4>
                  <p class="muted">Saves the current {selectedPaths().length} selected paths as a reusable profile in the user catalogue. The profile will appear in future discovery results for devices with matching environment and role.</p>
                  <label>
                    Profile name
                    <input
                      type="text"
                      bind:value={customProfileName}
                      placeholder="my_custom_dc_leaf"
                      pattern="[a-zA-Z0-9_-]+"
                    />
                    <small>Letters, digits, underscores, hyphens only.</small>
                  </label>
                  <div class="modal-actions">
                    <button class="ghost" onclick={() => showSaveCustomModal = false}>Cancel</button>
                    <button
                      onclick={saveAsCustomProfile}
                      disabled={savingCustom || !customProfileName.trim()}
                    >{savingCustom ? 'Saving...' : 'Save profile'}</button>
                  </div>
                </div>
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
      {#if devices.length}
        <div class="bulk-toolbar">
          <label class="select-all">
            <input
              type="checkbox"
              checked={selectedDeviceAddresses.length === devices.length}
              onchange={toggleAllDevices}
            />
            <span>{selectedDeviceAddresses.length} selected</span>
          </label>
          <button class="ghost" onclick={() => bulkDeviceAction('stop')} disabled={!selectedDeviceAddresses.length}>Stop selected</button>
          <button class="ghost" onclick={() => bulkDeviceAction('start')} disabled={!selectedDeviceAddresses.length}>Start selected</button>
          <button onclick={() => bulkDeviceAction('restart')} disabled={!selectedDeviceAddresses.length}>Restart selected</button>
        </div>
      {/if}

      {#if loading}
        <p class="empty">Loading managed devices...</p>
      {:else if !devices.length}
        <p class="empty">No managed devices yet. Add one in the wizard to start the subscriber lifecycle.</p>
      {:else}
        <div class="device-list">
          {#each devices as device}
            <article class="managed-device">
              <header>
                <input
                  class="device-select"
                  type="checkbox"
                  checked={selectedDeviceAddresses.includes(device.address)}
                  onchange={() => toggleDeviceSelection(device.address)}
                  aria-label={`Select ${device.address}`}
                />
                <div>
                  <h4>{device.hostname || device.address}</h4>
                  <p>
                    <span class="badge {device.enabled ? 'healthy' : 'critical'}">{device.enabled ? 'enabled' : 'stopped'}</span>
                    {device.address} - {device.vendor || 'vendor pending'} - {device.role || 'role unset'} - {device.credential_alias || 'env credentials'}
                  </p>
                </div>
                <div class="device-actions">
                  <button class="ghost" onclick={() => editDevice(device)}>Edit in wizard</button>
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
