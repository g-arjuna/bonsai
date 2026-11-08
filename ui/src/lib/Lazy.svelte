<script module>
  const componentCache = new Map();
</script>

<script>
  let { loader, ...rest } = $props();

  let Comp = $state(null);

  $effect(() => {
    const hit = componentCache.get(loader);
    if (hit) {
      Comp = hit;
      return;
    }
    let cancelled = false;
    loader().then((m) => {
      if (cancelled) return;
      componentCache.set(loader, m.default);
      Comp = m.default;
    });
    return () => { cancelled = true; };
  });
</script>

{#if Comp}
  <Comp {...rest} />
{:else}
  <div class="route-loading">Loading…</div>
{/if}

<style>
  .route-loading {
    padding: 2rem;
    color: var(--text-secondary, #9ca3af);
    font-size: 0.85rem;
  }
</style>
