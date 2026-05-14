<script>
  let { sidecars } = $props()

  // CV7 v1: ML inference lives bundled inside the `rules` sidecar; future
  // versions may split it out as a typed `ml-inference` sidecar. We surface
  // model status from either path.
  let mlSidecars = $derived(sidecars.filter(s =>
    s.kind === 'ml-inference' ||
    (s.kind === 'rules' && s.status_json && /ml_models_loaded/.test(s.status_json))
  ))

  function parseStatus(json) {
    try { return JSON.parse(json) } catch { return {} }
  }
</script>

{#if mlSidecars.length === 0}
  <p class="empty">No ML inference sidecar registered. Load a model into <code>models/</code> and restart the rules sidecar to surface it here.</p>
{:else}
  <div class="cards">
    {#each mlSidecars as s}
      {@const status = parseStatus(s.status_json)}
      <div class="ml-card">
        <h3>{s.name}</h3>
        <dl>
          <dt>kind</dt><dd>{s.kind}</dd>
          {#if status.ml_models_loaded !== undefined}
            <dt>models loaded</dt><dd>{status.ml_models_loaded}</dd>
          {/if}
          {#if status.model_paths}
            <dt>paths</dt>
            <dd>
              {#each status.model_paths as p}
                <div><code>{p}</code></div>
              {/each}
            </dd>
          {/if}
          {#if status.threshold !== undefined}
            <dt>threshold</dt><dd>{status.threshold}</dd>
          {/if}
        </dl>
      </div>
    {/each}
  </div>
{/if}

<style>
  .empty { color: var(--text-dim); }
  .empty code {
    background: var(--surface-2);
    padding: 2px 6px;
    border-radius: 3px;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 1rem;
  }
  .ml-card {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 1rem;
  }
  dl {
    display: grid;
    grid-template-columns: 6rem 1fr;
    gap: 0.25rem 0.5rem;
    margin: 0.5rem 0 0 0;
    font-size: 0.85rem;
  }
  dt { color: var(--text-dim); }
  dd { margin: 0; }
</style>
