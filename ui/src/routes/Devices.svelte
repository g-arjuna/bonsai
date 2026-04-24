<script>
  import { onMount } from 'svelte';
  import DeviceDrawer from '$lib/DeviceDrawer.svelte';
  import { navigate } from '$lib/router.svelte.js';
  import { toast } from '$lib/toast.svelte.js';

  function addDevice() {
    navigate('/devices/new');
  }

  let { selectedAddress = null } = $props();

  let devices = $state([]);
  let loading = $state(true);
  let selected = $state(null);

  $effect(() => {
    selected = selectedAddress;
  });

  onMount(loadDevices);

  async function loadDevices() {
    loading = true;
    try {
      const response = await fetch('/api/onboarding/devices');
      if (!response.ok) throw new Error(await response.text());
      const data = await response.json();
      devices = data.devices ?? [];
    } catch (error) {
      toast(error.message, 'error');
      devices = [];
    } finally {
      loading = false;
    }
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

  {#if loading}
    <div class="muted">Loading devices…</div>
  {:else if devices.length === 0}
    <div class="empty">No devices onboarded yet.</div>
  {:else}
    <div class="card">
      <table>
        <thead>
          <tr>
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
            <tr class:selected={selected === device.address} onclick={() => openDevice(device.address)}>
              <td>
                <strong>{device.hostname || device.address}</strong><br />
                <span class="muted device-address">{device.address}</span>
              </td>
              <td>{device.vendor || '—'}</td>
              <td>{device.role || '—'}</td>
              <td>{device.site || '—'}</td>
              <td>{device.collector_id || 'unassigned'}</td>
              <td>
                <span class="badge {device.enabled ? 'healthy' : 'critical'}">
                  {device.enabled ? 'enabled' : 'disabled'}
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
</style>
