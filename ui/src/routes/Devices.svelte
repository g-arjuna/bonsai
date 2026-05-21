<script>
  import { onMount } from 'svelte';
  import DeviceDrawer from '$lib/DeviceDrawer.svelte';
  import { navigate } from '$lib/router.svelte.js';
  import { toast } from '$lib/toast.svelte.js';

  function addDevice() {
    navigate('/devices/new');
  }

  let { selectedAddress = null } = $props();

  let devices     = $state([]);
  let loading     = $state(true);
  let selected    = $state(null);
  let incidentMap = $state(new Map());

  // ── D3-2 T5: per-device readiness badges ───────────────────────────────────
  let readinessMap = $state({});

  function readinessBadgeClass(r) {
    if (!r || r.loading) return 'info';
    if (r.error) return 'critical';
    if (r.service_status === 'reachable' && !r.blockers?.length) return 'healthy';
    if (r.service_status === 'reachable') return 'warn';
    return 'critical';
  }

  function readinessBadgeLabel(r) {
    if (!r || r.loading) return '…';
    if (r.error) return 'err';
    if (r.service_status === 'reachable' && !r.blockers?.length) return 'gNMI OK';
    if (r.service_status === 'reachable') return `⚠ ${r.blockers.length}`;
    if (r.service_status === 'auth_failed')  return 'auth fail';
    if (r.service_status === 'unreachable')  return 'unreachable';
    if (r.service_status === 'rpc_failed')   return 'RPC fail';
    return r.service_status || '?';
  }

  async function fetchReadiness(address) {
    readinessMap = { ...readinessMap, [address]: { loading: true } };
    try {
      const r = await fetch(`/api/devices/${encodeURIComponent(address)}/gnmi-readiness`);
      if (!r.ok) { readinessMap = { ...readinessMap, [address]: { loading: false, error: r.status } }; return; }
      const body = await r.json();
      const rpt  = body.report ?? {};
      readinessMap = { ...readinessMap, [address]: {
        loading:        false,
        service_status: rpt.service_status ?? 'unknown',
        blockers:       rpt.blockers ?? [],
      }};
    } catch (e) {
      readinessMap = { ...readinessMap, [address]: { loading: false, error: e.message } };
    }
  }

  // ── D3-2 T6: multi-device credential apply ─────────────────────────────────
  let selectedAddresses = $state(new Set());
  let credentials       = $state([]);
  let applyAlias        = $state('');
  let applying          = $state(false);

  function toggleSelect(address, e) {
    e.stopPropagation();
    const next = new Set(selectedAddresses);
    next.has(address) ? next.delete(address) : next.add(address);
    selectedAddresses = next;
  }

  function toggleSelectAll() {
    selectedAddresses = selectedAddresses.size === devices.length
      ? new Set()
      : new Set(devices.map(d => d.address));
  }

  async function applyCredential() {
    if (!applyAlias || !selectedAddresses.size) return;
    applying = true;
    let ok = 0, fail = 0;
    for (const address of selectedAddresses) {
      try {
        const r = await fetch('/api/onboarding/devices', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ address, credential_alias: applyAlias }),
        });
        const body = await r.json();
        body.success ? ok++ : fail++;
      } catch { fail++; }
    }
    applying = false;
    toast(`Credential applied to ${ok} device(s)${fail ? `, ${fail} failed` : ''}.`, fail ? 'warn' : 'success');
    selectedAddresses = new Set();
    applyAlias = '';
    await loadDevices();
  }

  $effect(() => { selected = selectedAddress; });

  onMount(loadDevices);

  async function loadDevices() {
    loading = true;
    readinessMap = {};
    try {
      const [devRes, incRes, credRes] = await Promise.all([
        fetch('/api/onboarding/devices'),
        fetch('/api/incidents'),
        fetch('/api/credentials'),
      ]);
      if (!devRes.ok) throw new Error(await devRes.text());
      const data = await devRes.json();
      devices = data.devices ?? [];

      if (incRes.ok) {
        const incData = await incRes.json();
        const map = new Map();
        for (const inc of incData.incidents ?? []) {
          const sev = inc.severity?.toLowerCase() ?? 'warn';
          for (const addr of inc.affected_devices ?? []) {
            const cur = map.get(addr);
            if (!cur || sev === 'critical') map.set(addr, sev);
          }
        }
        incidentMap = map;
      }

      if (credRes.ok) {
        const credData = await credRes.json();
        credentials = credData.credentials ?? [];
      }
    } catch (error) {
      toast(error.message, 'error');
      devices = [];
    } finally {
      loading = false;
    }

    // Fetch readiness in parallel (best-effort — don't block render)
    for (const d of devices) {
      fetchReadiness(d.address);
    }
  }

  function incidentSevClass(address) {
    const sev = incidentMap.get(address);
    return sev === 'critical' ? 'critical' : sev === 'warn' ? 'warn' : null;
  }

  function openDevice(address) {
    selected = address;
    navigate(`/devices/${encodeURIComponent(address)}`);
  }

  function closeDrawer() {
    selected = null;
    navigate('/devices');
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Inventory</p>
      <h2>Devices</h2>
    </div>
    <button class="primary" onclick={addDevice}>+ Add Device</button>
  </div>

  {#if selectedAddresses.size > 0}
    <div class="bulk-cred-bar">
      <span class="muted">{selectedAddresses.size} selected</span>
      <select bind:value={applyAlias}>
        <option value="">— apply credential —</option>
        {#each credentials as cred}
          <option value={cred.alias}>{cred.alias}</option>
        {/each}
      </select>
      <button onclick={applyCredential} disabled={applying || !applyAlias}>
        {applying ? 'Applying…' : 'Apply credential'}
      </button>
      <button class="ghost compact" onclick={() => selectedAddresses = new Set()}>Clear selection</button>
    </div>
  {/if}

  {#if loading}
    <div class="muted">Loading devices…</div>
  {:else if devices.length === 0}
    <div class="empty">No devices onboarded yet.</div>
  {:else}
    <div class="card">
      <table>
        <thead>
          <tr>
            <th class="col-check">
              <input type="checkbox"
                checked={selectedAddresses.size === devices.length && devices.length > 0}
                onchange={toggleSelectAll}
                title="Select all"
              />
            </th>
            <th>Device</th>
            <th>Vendor</th>
            <th>Role</th>
            <th>Site</th>
            <th>Collector</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          {#each devices as device (device.address)}
            {@const rdns = readinessMap[device.address]}
            <tr class:selected={selected === device.address} onclick={() => openDevice(device.address)}>
              <td class="col-check" onclick={(e) => toggleSelect(device.address, e)}>
                <input type="checkbox" checked={selectedAddresses.has(device.address)} onchange={() => {}} />
              </td>
              <td>
                <strong>{device.hostname || device.address}</strong><br />
                <span class="muted device-address">{device.address}</span>
              </td>
              <td>{device.vendor || '—'}</td>
              <td>{device.role || '—'}</td>
              <td>{device.site || '—'}</td>
              <td>{device.collector_id || 'unassigned'}</td>
              <td class="status-cell">
                <span class="badge {device.enabled ? 'healthy' : 'critical'}">
                  {device.enabled ? 'enabled' : 'disabled'}
                </span>
                {#if incidentSevClass(device.address)}
                  <span class="badge {incidentSevClass(device.address)}">
                    {incidentMap.get(device.address)} incident
                  </span>
                {/if}
                <span class="badge {readinessBadgeClass(rdns)}" title={rdns?.blockers?.join(', ') || ''}>
                  {readinessBadgeLabel(rdns)}
                </span>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

{#if selected}
  <DeviceDrawer address={selected} onclose={closeDrawer} />
{/if}

<style>
  tbody tr {
    cursor: pointer;
  }

  tbody tr.selected {
    background: rgba(88, 166, 255, 0.08);
  }

  .device-address {
    font-size: 12px;
  }

  .col-check {
    width: 36px;
    text-align: center;
  }

  .status-cell {
    display: flex;
    gap: 4px;
    align-items: center;
    flex-wrap: wrap;
  }

  .bulk-cred-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    margin-bottom: 12px;
    background: var(--bg-surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    flex-wrap: wrap;
  }

  .bulk-cred-bar select {
    padding: 4px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--text);
    font-size: 13px;
  }
</style>
