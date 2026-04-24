<script>
  import { navigate } from '$lib/router.svelte.js';
  import Topology from '$lib/Topology.svelte';
  import Events from '$lib/Events.svelte';
  import DeviceDrawer from '$lib/DeviceDrawer.svelte';

  let selectedDevice = $state(null);

  function onSelect(e) {
    selectedDevice = e.detail;
  }

  function onTrace(e) {
    navigate('/trace/' + encodeURIComponent(e.detail));
  }

  function closeDrawer() {
    selectedDevice = null;
  }
</script>

<div class="live-shell">
  <div class="live-pane live-topo">
    <Topology on:select={onSelect} />
  </div>
  <div class="live-pane live-events">
    <Events on:trace={onTrace} />
  </div>
</div>

{#if selectedDevice}
  <DeviceDrawer address={selectedDevice} onclose={closeDrawer} />
{/if}

<style>
  .live-shell {
    display: grid;
    grid-template-columns: 3fr 2fr;
    height: 100vh;
    overflow: hidden;
  }
  .live-pane { overflow-y: auto; }
  .live-topo { border-right: 1px solid var(--border); }
</style>
