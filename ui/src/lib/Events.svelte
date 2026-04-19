<script>
  import { onMount, createEventDispatcher } from 'svelte';

  const dispatch = createEventDispatcher();

  let events = $state([]);
  let paused = $state(false);
  let es;

  function fmt(ns) {
    return new Date(ns / 1e6).toISOString().replace('T', ' ').replace('Z', '');
  }

  onMount(() => {
    es = new EventSource('/api/events');
    es.onmessage = (e) => {
      if (paused) return;
      try {
        const ev = JSON.parse(e.data);
        events = [ev, ...events].slice(0, 200);
      } catch {}
    };
    es.onerror = () => { /* browser auto-reconnects SSE */ };
    return () => es.close();
  });
</script>

<div class="view">
  <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:12px;">
    <h2>Live Event Feed</h2>
    <div style="display:flex; gap:8px; align-items:center;">
      <span style="color:var(--muted); font-size:12px">{events.length} events</span>
      <button
        onclick={() => paused = !paused}
        style="background:none; border:1px solid var(--border); color:{paused ? 'var(--yellow)' : 'var(--muted)'}; padding:4px 12px; border-radius:4px; cursor:pointer;"
      >
        {paused ? 'Resume' : 'Pause'}
      </button>
      <button
        onclick={() => events = []}
        style="background:none; border:1px solid var(--border); color:var(--muted); padding:4px 12px; border-radius:4px; cursor:pointer;"
      >
        Clear
      </button>
    </div>
  </div>

  {#if !events.length}
    <p class="empty">Waiting for events... (bonsai SSE stream connected)</p>
  {:else}
    <div class="card" style="max-height: 600px; overflow-y: auto;">
      {#each events as ev}
        <div class="event-row">
          <span class="ts">{fmt(ev.occurred_at_ns)}</span>
          <div class="body">
            <span class="badge info">{ev.event_type}</span>
            <strong style="margin-left:8px">{ev.device_address}</strong>
            {#if ev.state_change_event_id}
              <button
                onclick={() => dispatch('trace', ev.state_change_event_id)}
                style="float:right; background:none; border:none; color:var(--blue); cursor:pointer; font-size:12px; padding:0;"
              >
                View trace →
              </button>
            {/if}
            <div style="color:var(--muted); font-size:12px; margin-top:4px; font-family:monospace; white-space:pre-wrap; word-break:break-all;">
              {ev.detail_json}
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>
