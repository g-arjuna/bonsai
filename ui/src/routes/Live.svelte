<script>
  import { navigate } from '$lib/router.svelte.js';
  import Topology      from '$lib/Topology.svelte';
  import Events        from '$lib/Events.svelte';
  import DeviceDrawer  from '$lib/DeviceDrawer.svelte';
  import SiteRail      from '$lib/SiteRail.svelte';
  import LiveStatusBar from '$lib/LiveStatusBar.svelte';

  let selectedDevice  = $state(null);
  let activeSite      = $state('');
  let sseConnected    = $state(false);
  let incidentDevices = $state(new Map());
  let topoSnapshot    = $state({ devices: [], links: [], host_endpoints: [] });
  let lastRefresh     = $state(null);

  // Incident count from topology's incident map
  const incidentCount = $derived(incidentDevices.size);

  function onSelect(e)     { selectedDevice = e.detail; }
  function onTrace(e)      { navigate('/trace/' + encodeURIComponent(e.detail)); }
  function closeDrawer()   { selectedDevice = null; }

  function onIncidentMapChange(map) {
    incidentDevices = map;
    lastRefresh = Date.now();
  }

  // Topology exposes its loaded data upward via a callback so the status bar
  // and site rail can read it without a second fetch.
  function onTopoLoad(topo) {
    topoSnapshot = topo;
    lastRefresh = Date.now();
  }
</script>

<div class="live-root">
  <LiveStatusBar
    topology={topoSnapshot}
    incidentCount={incidentCount}
    sseConnected={sseConnected}
    lastRefresh={lastRefresh}
  />

  <div class="live-body">
    <!-- Left: site drill-down rail -->
    <SiteRail
      topology={{ ...topoSnapshot, incidentDevices }}
      activeSite={activeSite}
      onSiteSelect={(s) => activeSite = s}
    />

    <!-- Centre: topology canvas (takes remaining space) -->
    <div class="topo-panel">
      <Topology
        activeSite={activeSite}
        onIncidentMapChange={onIncidentMapChange}
        onTopoLoad={onTopoLoad}
        on:select={onSelect}
      />
    </div>

    <!-- Right: live event feed -->
    <div class="events-panel">
      <Events
        onSseChange={(v) => sseConnected = v}
        on:trace={onTrace}
      />
    </div>
  </div>
</div>

{#if selectedDevice}
  <DeviceDrawer address={selectedDevice} onclose={closeDrawer} />
{/if}

<style>
  .live-root {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
  }

  .live-body {
    display: grid;
    grid-template-columns: 140px 1fr 320px;
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  .topo-panel {
    overflow: hidden;
    border-left:  1px solid var(--border-subtle, rgba(255,255,255,0.06));
    border-right: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    display: flex;
    flex-direction: column;
  }

  .events-panel {
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
</style>
