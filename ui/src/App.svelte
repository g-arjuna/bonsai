<script>
  import { onMount } from 'svelte';
  import Topology from './lib/Topology.svelte';
  import Events from './lib/Events.svelte';
  import Trace from './lib/Trace.svelte';
  import Onboarding from './lib/Onboarding.svelte';

  let view = $state('topology');
  let traceId = $state(null);

  function openTrace(id) {
    traceId = id;
    view = 'trace';
  }

  onMount(() => {
    const hash = location.hash.replace('#', '') || 'topology';
    if (['topology','onboarding','events','trace'].includes(hash)) view = hash;
  });

  function nav(v) {
    view = v;
    location.hash = v;
  }
</script>

<nav>
  <span class="brand">🌿 bonsai</span>
  <a class:active={view==='topology'} onclick={() => nav('topology')} href="#topology">Topology</a>
  <a class:active={view==='onboarding'} onclick={() => nav('onboarding')} href="#onboarding">Onboarding</a>
  <a class:active={view==='events'}   onclick={() => nav('events')}   href="#events">Events</a>
  {#if view === 'trace'}
    <a class:active={true} href="#trace">Trace</a>
  {/if}
</nav>

{#if view === 'topology'}
  <Topology on:trace={(e) => openTrace(e.detail)} />
{:else if view === 'onboarding'}
  <Onboarding />
{:else if view === 'events'}
  <Events on:trace={(e) => openTrace(e.detail)} />
{:else if view === 'trace'}
  <Trace id={traceId} />
{/if}
