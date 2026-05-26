<script>
  import { onMount } from 'svelte';
  import { navigate } from '$lib/router.svelte.js';
  import { toast } from '$lib/toast.svelte.js';

  // When first_run=true, prepend 4 setup steps (Welcome/Environment/Site/Credential)
  // before the device wizard steps. App.svelte passes this when is_first_run is detected.
  let { first_run = false, onComplete = () => {} } = $props();

  const SETUP_STEPS = [
    { id: 1, label: 'Welcome' },
    { id: 2, label: 'Resource Profile' },
    { id: 3, label: 'Environment' },
    { id: 4, label: 'Site' },
    { id: 5, label: 'Credential' },
    { id: 6, label: 'Vendor Defaults' },
  ];

  const DEVICE_STEPS = [
    { id: 1, label: 'Identity' },
    { id: 2, label: 'Discovery' },
    { id: 3, label: 'Paths' },
    { id: 4, label: 'Confirm' }
  ];

  // Logical steps exposed to the template
  let STEPS = $derived(first_run
    ? [...SETUP_STEPS, ...DEVICE_STEPS.map(s => ({ id: s.id + 6, label: s.label }))]
    : DEVICE_STEPS
  );

  // Offset applied to device wizard step numbers when first_run is active
  let stepOffset = $derived(first_run ? 6 : 0);

  // ── D4-7 T6: Blank-boot wizard — resource profile + vendor auto-load ───────
  const RESOURCE_PROFILES = [
    { value: 'low',      label: 'Low',      desc: '≤8 devices, 512 MB memory budget. Ideal for home labs.' },
    { value: 'standard', label: 'Standard', desc: '8–50 devices, 1 GB memory budget. Campus or small DC.' },
    { value: 'high',     label: 'High',     desc: '50+ devices, 2 GB+ memory budget. Production DC fabric.' },
  ];

  const VENDOR_DEFAULTS = [
    { value: 'nokia-srl',    label: 'Nokia SR Linux',  patterns: ['syslog_patterns/nokia-srlinux', 'path_profiles/nokia-srlinux', 'snmp_oid_patterns/default'] },
    { value: 'nokia-sros',   label: 'Nokia SR-OS',     patterns: ['syslog_patterns/nokia-sros', 'path_profiles/nokia-sros'] },
    { value: 'cisco-iosxr',  label: 'Cisco IOS-XR',    patterns: ['syslog_patterns/cisco-iosxr', 'path_profiles/cisco-iosxr'] },
    { value: 'cisco-iosxe',  label: 'Cisco IOS-XE',    patterns: ['syslog_patterns/cisco-iosxe', 'path_profiles/cisco-iosxe'] },
    { value: 'arista-eos',   label: 'Arista EOS',      patterns: ['syslog_patterns/arista-eos', 'path_profiles/arista-eos'] },
    { value: 'juniper-junos', label: 'Juniper JunOS',  patterns: ['syslog_patterns/juniper-junos', 'path_profiles/juniper-junos'] },
    { value: 'frr',          label: 'FRRouting',       patterns: ['syslog_patterns/frr'] },
  ];

  let frResourceProfile = $state('standard');
  let frResourceSaving  = $state(false);
  let frSelectedVendors = $state([]);
  let frVendorLoading   = $state(false);
  let frVendorDone      = $state(false);

  async function frSaveResourceProfile() {
    frResourceSaving = true;
    try {
      const r = await fetch('/api/governance/profile', {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ profile: frResourceProfile }),
      });
      if (!r.ok) throw new Error(await r.text());
      toast(`Resource profile set to "${frResourceProfile}".`, 'success');
      step = 3;
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      frResourceSaving = false;
    }
  }

  function frToggleVendor(v) {
    if (frSelectedVendors.includes(v)) {
      frSelectedVendors = frSelectedVendors.filter(x => x !== v);
    } else {
      frSelectedVendors = [...frSelectedVendors, v];
    }
  }

  async function frLoadVendorDefaults() {
    if (!frSelectedVendors.length) { step = 7; return; }
    frVendorLoading = true;
    frVendorDone = false;
    try {
      // Enable the matching config items in the DB
      for (const vendor of frSelectedVendors) {
        const vd = VENDOR_DEFAULTS.find(v => v.value === vendor);
        if (!vd) continue;
        // Fetch all config items and enable those matching this vendor's patterns
        const r = await fetch('/api/config-items');
        if (!r.ok) continue;
        const items = await r.json();
        for (const item of items) {
          const matchesVendor = vd.patterns.some(p => item.id.includes(vendor) || item.id.includes(p));
          if (matchesVendor && !item.enabled) {
            await fetch('/api/config-items', {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ ...item, enabled: true }),
            });
          }
        }
      }
      frVendorDone = true;
      toast(`Enabled default patterns for ${frSelectedVendors.length} vendor(s).`, 'success');
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      frVendorLoading = false;
    }
  }

  // ── First-run setup state ─────────────────────────────────────────────────
  const ARCHETYPES = [
    { value: 'data_center',      label: 'Data Center',      desc: 'DC fabrics, spine/leaf, EVPN/BGP' },
    { value: 'campus_wired',     label: 'Campus Wired',     desc: 'Access/distribution/core LAN' },
    { value: 'campus_wireless',  label: 'Campus Wireless',  desc: 'APs, WLCs, wireless overlay' },
    { value: 'service_provider', label: 'Service Provider', desc: 'Core, PE/P routers, MPLS, SR' },
    { value: 'home_lab',         label: 'Home Lab',         desc: 'ContainerLab, FRR, any topology' },
  ];

  let frEnvName      = $state('');
  let frEnvArchetype = $state('home_lab');
  let frEnvSaving    = $state(false);
  let frEnvCreated   = $state(null);

  let frSiteName     = $state('');
  let frSiteKind     = $state('dc');
  let frSiteSaving   = $state(false);
  let frSiteCreated  = $state(null);

  let frCredAlias    = $state('');
  let frCredUser     = $state('');
  let frCredPass     = $state('');
  let frCredSaving   = $state(false);
  let frCredCreated  = $state(null);

  async function frCreateEnvironment() {
    if (!frEnvName.trim()) return;
    frEnvSaving = true;
    try {
      const r = await fetch('/api/environments', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: frEnvName.trim(), archetype: frEnvArchetype }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      frEnvCreated = { name: frEnvName.trim(), archetype: frEnvArchetype };
      toast(`Environment "${frEnvName.trim()}" created.`, 'success');
      step = 3;
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      frEnvSaving = false;
    }
  }

  async function frCreateSite() {
    if (!frSiteName.trim()) return;
    frSiteSaving = true;
    try {
      const r = await fetch('/api/sites', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: frSiteName.trim(), kind: frSiteKind, parent_id: '', lat: 0, lon: 0, metadata_json: '{}' }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      frSiteCreated = { name: frSiteName.trim(), kind: frSiteKind };
      toast(`Site "${frSiteName.trim()}" created.`, 'success');
      await loadSites();
      step = 4;
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      frSiteSaving = false;
    }
  }

  async function frCreateCredential() {
    if (!frCredAlias.trim() || !frCredUser.trim() || !frCredPass) return;
    frCredSaving = true;
    try {
      const r = await fetch('/api/credentials', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ alias: frCredAlias.trim(), username: frCredUser.trim(), password: frCredPass }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      frCredCreated = { alias: frCredAlias.trim() };
      form.credential_alias = frCredAlias.trim();
      toast(`Credential alias "${frCredAlias.trim()}" stored in vault.`, 'success');
      await loadCredentials();
      step = 6;  // vendor defaults
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      frCredSaving = false;
    }
  }

  async function frSkip() {
    try {
      await fetch('/api/environments', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: 'Home Lab', archetype: 'home_lab' }),
      });
      await fetch('/api/sites', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: 'default-site', kind: 'other', parent_id: '', lat: 0, lon: 0, metadata_json: '{}' }),
      });
      await loadSites();
      await loadEnvironments();
    } catch (_) { /* non-fatal */ }
    toast('Setup skipped. Add devices from the wizard.', 'info');
    onComplete();
    navigate('/');
  }

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

  // ── NetBox import (D3-2 T2) ──────────────────────────────────────────────────
  // ── Per-device readiness badges (D3-2 T5) ────────────────────────────────────
  // Map of address → { service_status, tls_status, blockers, loading }
  let deviceReadiness = $state({});

  async function fetchReadinessBadge(address) {
    deviceReadiness = { ...deviceReadiness, [address]: { loading: true } };
    try {
      const r = await fetch(`/api/devices/${encodeURIComponent(address)}/gnmi-readiness`);
      if (!r.ok) {
        deviceReadiness = { ...deviceReadiness, [address]: { loading: false, error: r.status } };
        return;
      }
      const body = await r.json();
      const rpt  = body.report || {};
      deviceReadiness = { ...deviceReadiness, [address]: {
        loading:        false,
        service_status: rpt.service_status || 'unknown',
        tls_status:     rpt.tls_status     || 'unknown',
        blockers:       rpt.blockers        || [],
        actions:        rpt.recommended_actions || [],
      }};
    } catch (e) {
      deviceReadiness = { ...deviceReadiness, [address]: { loading: false, error: e.message } };
    }
  }

  function readinessBadgeClass(r) {
    if (!r || r.loading) return 'info';
    if (r.error) return 'critical';
    if (r.service_status === 'reachable' && !r.blockers?.length) return 'healthy';
    if (r.service_status === 'reachable') return 'degraded';
    return 'critical';
  }

  function readinessBadgeLabel(r) {
    if (!r || r.loading) return 'checking…';
    if (r.error) return 'readiness error';
    if (r.service_status === 'reachable' && !r.blockers?.length) return 'gNMI OK';
    if (r.service_status === 'reachable') return `gNMI ⚠ ${r.blockers.length} blocker${r.blockers.length === 1 ? '' : 's'}`;
    if (r.service_status === 'rpc_failed') return 'RPC failed';
    if (r.service_status === 'unreachable') return 'unreachable';
    if (r.service_status === 'auth_failed') return 'auth failed';
    return r.service_status;
  }

  // ── Inline add-credential expander (D3-2 T3) ────────────────────────────────
  let showInlineCredForm = $state(false);
  let inlineCredForm = $state({ alias: '', username: '', password: '' });
  let inlineCredSaving = $state(false);

  async function saveInlineCredential() {
    if (!inlineCredForm.alias.trim() || !inlineCredForm.username.trim() || !inlineCredForm.password) return;
    inlineCredSaving = true;
    try {
      const r = await fetch('/api/credentials', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(inlineCredForm),
      });
      if (!r.ok) throw new Error(await r.text());
      const body = await r.json();
      if (!body.success) throw new Error(body.error || 'credential save failed');
      form.credential_alias = body.credential.alias;
      inlineCredForm = { alias: '', username: '', password: '' };
      showInlineCredForm = false;
      invalidateDiscovery();
      await loadCredentials();
      toast(`Credential "${body.credential.alias}" stored in vault.`, 'success');
    } catch (e) {
      error = e.message;
    } finally {
      inlineCredSaving = false;
    }
  }

  // ── Bulk CSV/JSON import (D3-2 T4) ─────────────────────────────────────────
  let bulkImportText    = $state('');
  let bulkImportMode    = $state('csv'); // 'csv' | 'json'
  let bulkImportResults = $state([]);
  let bulkImporting     = $state(false);

  const CSV_HEADER = 'address,hostname,vendor,role,site,credential_alias';
  const CSV_PLACEHOLDER = `address,hostname,vendor,role,site,credential_alias
192.0.2.1,router-1,nokia-srl,spine,dc-london,lab-creds
192.0.2.2,router-2,frr,leaf,dc-london,lab-creds`;

  function parseCsvImport(text) {
    const lines = text.trim().split('\n').filter(l => l.trim() && !l.startsWith('#'));
    if (!lines.length) return [];
    const first = lines[0].trim().toLowerCase();
    const hasHeader = first.startsWith('address');
    const rows = hasHeader ? lines.slice(1) : lines;
    return rows.map(line => {
      const [address = '', hostname = '', vendor = '', role = '', site = '', credential_alias = ''] =
        line.split(',').map(v => v.trim());
      return { address, hostname, vendor, role, site, credential_alias };
    }).filter(r => r.address);
  }

  async function runBulkImport() {
    const items = bulkImportMode === 'json'
      ? (() => { try { return JSON.parse(bulkImportText); } catch { error = 'Invalid JSON'; return null; } })()
      : parseCsvImport(bulkImportText);
    if (!items || !items.length) { error = 'No valid rows found.'; return; }
    bulkImporting = true;
    bulkImportResults = [];
    error = '';
    try {
      const r = await fetch('/api/onboarding/import', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(items),
      });
      if (!r.ok) throw new Error(await r.text());
      const body = await r.json();
      bulkImportResults = body.results || [];
      message = `Imported ${body.imported} device(s)${body.failed ? `, ${body.failed} failed` : ''}.`;
      if (body.imported) await loadDevices();
    } catch (e) {
      error = e.message;
    } finally {
      bulkImporting = false;
    }
  }

  let nbUrl         = $state('');
  let nbToken       = $state('');
  let nbSiteSlug    = $state('');
  let nbFetching    = $state(false);
  let nbCandidates  = $state([]);
  let nbSelected    = $state([]);
  let nbVersion     = $state('');
  let nbWarnings    = $state([]);
  let nbImporting   = $state(false);
  let nbImportDone  = $state([]);

  async function nbFetchDevices() {
    nbFetching = true;
    nbCandidates = [];
    nbSelected = [];
    nbWarnings = [];
    nbVersion = '';
    error = '';
    try {
      const r = await fetch('/api/enrichment/netbox/import', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ url: nbUrl.trim(), token: nbToken.trim(), site_slug: nbSiteSlug.trim() }),
      });
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      nbCandidates = data.candidates || [];
      nbSelected = data.candidates.map(c => c.address);
      nbVersion = data.netbox_version || '';
      nbWarnings = data.warnings || [];
    } catch (e) {
      error = e.message;
    } finally {
      nbFetching = false;
    }
  }

  function nbToggle(address) {
    if (nbSelected.includes(address)) {
      nbSelected = nbSelected.filter(a => a !== address);
    } else {
      nbSelected = [...nbSelected, address];
    }
  }

  async function nbImportSelected() {
    const toImport = nbCandidates.filter(c => nbSelected.includes(c.address));
    if (!toImport.length) return;
    nbImporting = true;
    nbImportDone = [];
    error = '';
    // Use the first available credential alias (if any) as a hint
    const credHint = credentials[0]?.alias || '';
    const results = [];
    for (const dev of toImport) {
      try {
        const r = await fetch('/api/onboarding/devices/with_paths', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({
            address: dev.address,
            hostname: dev.name,
            vendor: dev.vendor,
            role: dev.role || 'leaf',
            site: dev.site,
            enabled: true,
            credential_alias: credHint,
            username_env: '',
            password_env: '',
            tls_domain: '',
            ca_cert: '',
            selected_paths: [],
          }),
        });
        const body = await r.json();
        results.push({ address: dev.address, ok: body.success, msg: body.error || '' });
      } catch (e) {
        results.push({ address: dev.address, ok: false, msg: e.message });
      }
    }
    nbImportDone = results;
    nbImporting = false;
    await loadDevices();
  }

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
      fetchReadinessBadge(body.device.address);
      if (first_run) { onComplete(); navigate('/'); return; }
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
    const identityStep  = 1 + stepOffset;
    const discoveryStep = 2 + stepOffset;
    const pathsStep     = 3 + stepOffset;
    const maxStep       = 4 + stepOffset;
    if (step === identityStep && !form.address.trim()) {
      error = 'gNMI address is required before discovery.';
      return;
    }
    if (step === discoveryStep && !discovery) {
      error = 'Run discovery before choosing a path profile.';
      return;
    }
    if (step === pathsStep && !selectedPaths().length) {
      error = 'Select at least one subscription path before confirming.';
      return;
    }
    step = Math.min(maxStep, step + 1);
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
      <button class:active={workspace === 'netbox'} onclick={() => workspace = 'netbox'}>Import from NetBox</button>
      <button class:active={workspace === 'import'} onclick={() => workspace = 'import'}>Bulk import</button>
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

        {#if first_run && step === 1}
          <div class="panel-heading">
            <p class="eyebrow">Welcome</p>
            <h3>Welcome to bonsai</h3>
            <p class="muted">
              A streaming-first network state engine. bonsai ingests gNMI telemetry, builds a
              live graph of your network, and closes a detect-predict-heal loop.
            </p>
            <p class="muted">
              This wizard takes about two minutes to configure your first environment, site, and
              device credential. You can also skip it and configure everything from the workspaces.
            </p>
          </div>
          <div class="wizard-actions">
            <button type="button" onclick={() => step = 2}>Get started</button>
            <button type="button" class="ghost" onclick={frSkip}>Skip, go to dashboard</button>
          </div>

        {:else if first_run && step === 2}
          <div class="panel-heading">
            <p class="eyebrow">Step 2 of 10</p>
            <h3>Choose a resource profile</h3>
            <p class="muted">
              The resource profile controls memory budget, write batch sizes, and rate shedding thresholds.
              Pick one that matches your deployment scale. You can change it later from the Governance page.
            </p>
          </div>
          <div class="archetype-grid">
            {#each RESOURCE_PROFILES as rp}
              <label class="archetype-option" class:selected={frResourceProfile === rp.value}>
                <input type="radio" name="fr-resource-profile" value={rp.value} bind:group={frResourceProfile} />
                <strong>{rp.label}</strong>
                <span class="muted small">{rp.desc}</span>
              </label>
            {/each}
          </div>
          <div class="wizard-actions">
            <button type="button" class="ghost" onclick={() => step = 1}>Back</button>
            <button type="button" onclick={frSaveResourceProfile} disabled={frResourceSaving}>
              {frResourceSaving ? 'Saving…' : 'Set profile & continue'}
            </button>
            <button type="button" class="ghost" onclick={() => step = 3}>Skip</button>
          </div>

        {:else if first_run && step === 3}
          <div class="panel-heading">
            <p class="eyebrow">Step 3 of 10</p>
            <h3>Define an environment</h3>
            <p class="muted">
              An environment groups sites and devices by operational context.
              The archetype shapes default role options and remediation trust levels.
            </p>
          </div>
          <div class="form-grid">
            <div class="form-row span-2">
              <label for="fr-env-name">Name</label>
              <input id="fr-env-name" bind:value={frEnvName} placeholder="e.g. Lab DC Fabric" autocomplete="off" />
            </div>
            <div class="form-row span-2">
              <label>Archetype</label>
              <div class="archetype-grid">
                {#each ARCHETYPES as arch}
                  <label class="archetype-option" class:selected={frEnvArchetype === arch.value}>
                    <input type="radio" name="fr-archetype" value={arch.value} bind:group={frEnvArchetype} />
                    <strong>{arch.label}</strong>
                    <span class="muted small">{arch.desc}</span>
                  </label>
                {/each}
              </div>
            </div>
          </div>
          <div class="wizard-actions">
            <button type="button" class="ghost" onclick={() => step = 2}>Back</button>
            <button type="button" onclick={frCreateEnvironment} disabled={frEnvSaving || !frEnvName.trim()}>
              {frEnvSaving ? 'Creating…' : 'Create environment'}
            </button>
            <button type="button" class="ghost" onclick={() => step = 4}>Skip this step</button>
          </div>

        {:else if first_run && step === 4}
          <div class="panel-heading">
            <p class="eyebrow">Step 4 of 10</p>
            <h3>Add a top-level site</h3>
            {#if frEnvCreated}
              <p class="muted">Environment <strong>{frEnvCreated.name}</strong> created. Now define your first site.</p>
            {:else}
              <p class="muted">Define at least one site — a data centre, PoP, campus, or region. You can build the full hierarchy later from the Sites workspace.</p>
            {/if}
          </div>
          <div class="form-grid">
            <div class="form-row">
              <label for="fr-site-name">Site name</label>
              <input id="fr-site-name" bind:value={frSiteName} placeholder="e.g. dc-lab-01" autocomplete="off" />
            </div>
            <div class="form-row">
              <label for="fr-site-kind">Kind</label>
              <select id="fr-site-kind" bind:value={frSiteKind}>
                <option value="region">region</option>
                <option value="dc">dc</option>
                <option value="pod">pod</option>
                <option value="rack">rack</option>
                <option value="other">other</option>
              </select>
            </div>
          </div>
          <div class="wizard-actions">
            <button type="button" class="ghost" onclick={() => step = 3}>Back</button>
            <button type="button" onclick={frCreateSite} disabled={frSiteSaving || !frSiteName.trim()}>
              {frSiteSaving ? 'Creating…' : 'Create site'}
            </button>
            <button type="button" class="ghost" onclick={() => step = 5}>Skip this step</button>
          </div>

        {:else if first_run && step === 5}
          <div class="panel-heading">
            <p class="eyebrow">Step 5 of 10</p>
            <h3>Store a device credential</h3>
            {#if frSiteCreated}
              <p class="muted">Site <strong>{frSiteCreated.name}</strong> created. Now store a credential alias. The username and password are encrypted in the local vault and never exposed in the API or UI.</p>
            {:else}
              <p class="muted">Store a credential alias. The username and password are encrypted in the local vault and never exposed in the API or UI.</p>
            {/if}
          </div>
          <div class="form-grid">
            <div class="form-row">
              <label for="fr-cred-alias">Alias</label>
              <input id="fr-cred-alias" bind:value={frCredAlias} placeholder="e.g. lab-admin" autocomplete="off" />
            </div>
            <div class="form-row">
              <label for="fr-cred-user">Username</label>
              <input id="fr-cred-user" bind:value={frCredUser} placeholder="admin" autocomplete="off" />
            </div>
            <div class="form-row">
              <label for="fr-cred-pass">Password</label>
              <input id="fr-cred-pass" type="password" bind:value={frCredPass} placeholder="••••••••" autocomplete="new-password" />
            </div>
          </div>
          <div class="wizard-actions">
            <button type="button" class="ghost" onclick={() => step = 4}>Back</button>
            <button type="button" onclick={frCreateCredential} disabled={frCredSaving || !frCredAlias.trim() || !frCredUser.trim() || !frCredPass}>
              {frCredSaving ? 'Saving…' : 'Save credential'}
            </button>
            <button type="button" class="ghost" onclick={() => step = 6}>Skip this step</button>
          </div>

        {:else if first_run && step === 6}
          <div class="panel-heading">
            <p class="eyebrow">Step 6 of 10</p>
            <h3>Select your vendors</h3>
            <p class="muted">
              Choose which network vendors are in your environment. Bonsai will enable the matching
              syslog patterns, gNMI path profiles, and SNMP OID patterns from the bundled defaults.
            </p>
          </div>
          <div class="vendor-grid">
            {#each VENDOR_DEFAULTS as vd}
              <label class="vendor-option" class:selected={frSelectedVendors.includes(vd.value)}>
                <input type="checkbox" checked={frSelectedVendors.includes(vd.value)} onchange={() => frToggleVendor(vd.value)} />
                <strong>{vd.label}</strong>
                <span class="muted small">{vd.patterns.length} pattern set{vd.patterns.length !== 1 ? 's' : ''}</span>
              </label>
            {/each}
          </div>
          {#if frVendorDone}
            <p class="success-msg">Vendor defaults loaded. Proceed to add your first device.</p>
          {/if}
          <div class="wizard-actions">
            <button type="button" class="ghost" onclick={() => step = 5}>Back</button>
            {#if frVendorDone}
              <button type="button" onclick={() => step = 7}>Add first device</button>
            {:else}
              <button type="button" onclick={frLoadVendorDefaults} disabled={frVendorLoading}>
                {frVendorLoading ? 'Loading…' : frSelectedVendors.length ? 'Load defaults & continue' : 'Skip — add device'}
              </button>
            {/if}
            <button type="button" class="ghost" onclick={() => step = 7}>Skip</button>
          </div>

        {:else if step === 1 + stepOffset}
          <div class="panel-heading">
            <p class="eyebrow">{first_run ? 'Step 7 of 10' : 'Step 1'}</p>
            <h3>{editingDeviceAddress ? 'Review address and credentials' : 'Address and credentials'}</h3>
            <p class="muted">Vault aliases are preferred. Env vars remain available for lab compatibility, but secrets never enter the registry JSON.</p>
          </div>

          <div class="form-grid">
            <div class="form-row span-2">
              <label for="onboard-address">Device address <span style="font-size:11px;font-weight:400;color:var(--text-muted,#888);margin-left:4px">(IP or hostname — no port)</span></label>
              <input id="onboard-address" bind:value={form.address} oninput={invalidateDiscovery} placeholder="172.100.102.12" required />
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
              <label for="onboard-credential-alias">
                Credential alias
                {#if !vaultUnlocked}<span class="vault-locked-chip">vault locked</span>{/if}
              </label>
              <div class="cred-picker-row">
                <select id="onboard-credential-alias" bind:value={form.credential_alias} onchange={invalidateDiscovery} disabled={!vaultUnlocked && !credentials.length}>
                  <option value="">{vaultUnlocked ? 'No vault alias' : credentials.length ? 'Pick alias (vault locked — existing aliases work)' : 'Vault locked'}</option>
                  {#each credentials as credential}
                    <option value={credential.alias}>{credential.alias}{credential.device_count ? ` (${credential.device_count} device${credential.device_count === 1 ? '' : 's'})` : ''}</option>
                  {/each}
                </select>
                {#if vaultUnlocked}
                  <button type="button" class="ghost compact" onclick={() => { showInlineCredForm = !showInlineCredForm; }}>
                    {showInlineCredForm ? 'Cancel' : '+ New'}
                  </button>
                {/if}
              </div>
              {#if showInlineCredForm}
                <div class="inline-cred-form">
                  <input bind:value={inlineCredForm.alias} placeholder="alias" autocomplete="off" />
                  <input bind:value={inlineCredForm.username} placeholder="username" autocomplete="off" />
                  <input bind:value={inlineCredForm.password} type="password" placeholder="password" autocomplete="new-password" />
                  <button type="button" onclick={saveInlineCredential} disabled={inlineCredSaving || !inlineCredForm.alias.trim() || !inlineCredForm.username.trim() || !inlineCredForm.password}>
                    {inlineCredSaving ? 'Saving…' : 'Store'}
                  </button>
                </div>
              {/if}
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
        {:else if step === 2 + stepOffset}
          <div class="panel-heading">
            <p class="eyebrow">{first_run ? 'Step 8 of 10' : 'Step 2'}</p>
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
                  <div class="blocker-row">
                    <p class="warning">{warning}</p>
                    {#if warning.toLowerCase().includes('tls') && warning.toLowerCase().includes('domain')}
                      <button class="cta-action" onclick={() => { step = 1 + stepOffset; document.getElementById('onboard-tls-domain')?.focus(); }}>Fix TLS domain</button>
                    {:else if warning.toLowerCase().includes('ca cert') || warning.toLowerCase().includes('ca_cert')}
                      <button class="cta-action" onclick={() => { step = 1 + stepOffset; document.getElementById('onboard-ca-cert')?.focus(); }}>Set CA cert path</button>
                    {:else if warning.toLowerCase().includes('auth') || warning.toLowerCase().includes('credential')}
                      <button class="cta-action" onclick={() => { step = 1 + stepOffset; document.getElementById('onboard-credential-alias')?.focus(); }}>Check credential</button>
                    {:else if warning.toLowerCase().includes('timeout') || warning.toLowerCase().includes('unreachable')}
                      <button class="cta-action" onclick={() => { step = 1 + stepOffset; document.getElementById('onboard-address')?.focus(); }}>Check address</button>
                    {:else if warning.toLowerCase().includes('tls') || warning.toLowerCase().includes('certificate')}
                      <button class="cta-action" onclick={() => { step = 1 + stepOffset; document.getElementById('onboard-ca-cert')?.focus(); }}>Review TLS config</button>
                    {/if}
                  </div>
                {/each}
              </div>
            {/if}
          {:else}
            <p class="empty">No report yet. Run discovery to unlock path profile selection.</p>
          {/if}
        {:else if step === 3 + stepOffset}
          <div class="panel-heading">
            <p class="eyebrow">{first_run ? 'Step 9 of 10' : 'Step 3'}</p>
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
            <p class="eyebrow">{first_run ? 'Step 10 of 10' : 'Step 4'}</p>
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

        {#if !first_run || step > 4}
          <div class="wizard-actions">
            <button type="button" class="ghost" onclick={previousStep} disabled={step === (first_run ? 5 : 1)}>Back</button>
            <button type="button" class="ghost" onclick={resetForm}>Clear</button>
            {#if step < 4 + stepOffset}
              <button type="button" onclick={nextStep}>Next</button>
            {:else}
              <button type="button" onclick={saveDevice} disabled={saving || !selectedPaths().length}>
                {saving ? 'Saving...' : 'Save and subscribe'}
              </button>
            {/if}
          </div>
        {/if}
      </div>
    </section>
  {:else if workspace === 'netbox'}
    <section class="managed-section separate-workspace">
      <div class="section-title">
        <h3>Import from NetBox</h3>
        <span>Pull active devices from your NetBox instance into the onboarding registry</span>
      </div>

      <div class="netbox-import-form">
        <div class="form-grid">
          <div class="form-row">
            <label for="nb-url">NetBox URL</label>
            <input id="nb-url" bind:value={nbUrl} placeholder="https://netbox.example.com" autocomplete="off" />
          </div>
          <div class="form-row">
            <label for="nb-token">API token</label>
            <input id="nb-token" bind:value={nbToken} type="password" placeholder="Token from NetBox → Profile → API Tokens" autocomplete="off" />
          </div>
          <div class="form-row">
            <label for="nb-site">Site slug <span class="muted">(optional)</span></label>
            <input id="nb-site" bind:value={nbSiteSlug} placeholder="e.g. dc-london" autocomplete="off" />
          </div>
        </div>
        <div class="wizard-actions">
          <button onclick={nbFetchDevices} disabled={nbFetching || !nbUrl.trim() || !nbToken.trim()}>
            {nbFetching ? 'Fetching…' : 'Fetch devices'}
          </button>
        </div>
      </div>

      {#if nbVersion}
        <p class="muted" style="margin-bottom: 8px;">NetBox <strong>{nbVersion}</strong> — {nbCandidates.length} device{nbCandidates.length === 1 ? '' : 's'} found</p>
      {/if}

      {#if nbWarnings.length}
        <div class="warning-stack" style="margin-bottom: 12px;">
          {#each nbWarnings as w}<p class="warning">{w}</p>{/each}
        </div>
      {/if}

      {#if nbCandidates.length}
        <div class="nb-candidate-list">
          <div class="nb-candidate-header">
            <label class="select-all">
              <input type="checkbox"
                checked={nbSelected.length === nbCandidates.length}
                onchange={() => nbSelected = nbSelected.length === nbCandidates.length ? [] : nbCandidates.map(c => c.address)}
              />
              <span>{nbSelected.length} of {nbCandidates.length} selected</span>
            </label>
            <button onclick={nbImportSelected} disabled={nbImporting || !nbSelected.length}>
              {nbImporting ? 'Importing…' : `Import ${nbSelected.length} device${nbSelected.length === 1 ? '' : 's'}`}
            </button>
          </div>

          {#each nbCandidates as cand}
            {@const done = nbImportDone.find(d => d.address === cand.address)}
            <div class="nb-candidate-row" class:nb-done={done?.ok} class:nb-fail={done && !done.ok}>
              <label class="nb-check">
                <input type="checkbox" checked={nbSelected.includes(cand.address)} onchange={() => nbToggle(cand.address)} />
              </label>
              <div class="nb-meta">
                <strong>{cand.name}</strong>
                <code>{cand.address}</code>
                <span class="muted">{cand.site} · {cand.role || 'role unknown'} · {cand.vendor || 'vendor unknown'}</span>
              </div>
              {#if done}
                <span class="badge {done.ok ? 'healthy' : 'critical'}">{done.ok ? 'imported' : done.msg || 'failed'}</span>
              {/if}
            </div>
          {/each}
        </div>
      {:else if nbVersion}
        <p class="empty">No active devices with a primary IP found.</p>
      {/if}
    </section>
  {:else if workspace === 'import'}
    <section class="managed-section separate-workspace">
      <div class="section-title">
        <h3>Bulk import</h3>
        <span>Paste a CSV or JSON array to onboard multiple devices at once</span>
      </div>

      <div class="bulk-import-tabs">
        <button class:active={bulkImportMode === 'csv'} onclick={() => bulkImportMode = 'csv'}>CSV</button>
        <button class:active={bulkImportMode === 'json'} onclick={() => bulkImportMode = 'json'}>JSON</button>
      </div>

      {#if bulkImportMode === 'csv'}
        <p class="muted" style="margin-bottom:6px;">Columns: <code>{CSV_HEADER}</code> — header row optional</p>
        <textarea
          class="bulk-import-area"
          bind:value={bulkImportText}
          placeholder={CSV_PLACEHOLDER}
          rows="8"
          spellcheck="false"
        ></textarea>
      {:else}
        <p class="muted" style="margin-bottom:6px;">Array of objects with keys: address, hostname, vendor, role, site, credential_alias</p>
        <textarea
          class="bulk-import-area"
          bind:value={bulkImportText}
          placeholder={'[{"address":"192.0.2.1","hostname":"router-1","vendor":"nokia-srl","role":"spine","site":"dc-london","credential_alias":"lab-creds"}]'}
          rows="8"
          spellcheck="false"
        ></textarea>
      {/if}

      <div class="wizard-actions" style="margin-top:12px;">
        <button onclick={runBulkImport} disabled={bulkImporting || !bulkImportText.trim()}>
          {bulkImporting ? 'Importing…' : 'Import devices'}
        </button>
        <button class="ghost" onclick={() => { bulkImportText = ''; bulkImportResults = []; }}>Clear</button>
      </div>

      {#if bulkImportResults.length}
        <table class="bulk-results-table" style="margin-top:16px;">
          <thead><tr><th>Address</th><th>Status</th><th>Detail</th></tr></thead>
          <tbody>
            {#each bulkImportResults as row}
              <tr>
                <td><code>{row.address}</code></td>
                <td><span class="badge {row.success ? 'healthy' : 'critical'}">{row.success ? 'imported' : 'failed'}</span></td>
                <td class="muted">{row.error || ''}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
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
            {@const rd = deviceReadiness[device.address]}
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
                    {#if rd !== undefined}
                      <span class="badge {readinessBadgeClass(rd)}" title={rd.blockers?.join('\n') || ''}>{readinessBadgeLabel(rd)}</span>
                    {:else}
                      <button class="readiness-probe-btn" onclick={() => fetchReadinessBadge(device.address)}>Check gNMI</button>
                    {/if}
                    <span class="muted-inline">{device.address} · {device.vendor || 'vendor pending'} · {device.role || 'role unset'} · {device.credential_alias || 'env credentials'}</span>
                  </p>
                  {#if rd?.blockers?.length}
                    <ul class="readiness-blockers">
                      {#each rd.blockers as b}<li>{b}</li>{/each}
                    </ul>
                  {/if}
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
