<script>
  import { onMount, onDestroy } from 'svelte'
  import StatusBanner from './lib/StatusBanner.svelte'
  import SidecarCard from './lib/SidecarCard.svelte'
  import RuleFiringTable from './lib/RuleFiringTable.svelte'
  import MlModelPanel from './lib/MlModelPanel.svelte'

  let sidecars = $state([])
  let required_kinds = $state([])
  let missing_required = $state(null)
  let detections = $state([])
  let last_fetch_ok = $state(true)
  let last_error = $state('')
  let interval

  async function refresh() {
    try {
      const [s, d] = await Promise.all([
        fetch('/api/sidecars').then(r => r.json()),
        fetch('/api/detections').then(r => r.ok ? r.json() : { detections: [] }).catch(() => ({ detections: [] })),
      ])
      sidecars = s.sidecars || []
      required_kinds = s.required_kinds || []
      missing_required = s.missing_required ?? null
      detections = (d.detections || d || []).slice(0, 200)
      last_fetch_ok = true
      last_error = ''
    } catch (e) {
      last_fetch_ok = false
      last_error = String(e)
    }
  }

  onMount(() => {
    refresh()
    interval = setInterval(refresh, 5000)
  })
  onDestroy(() => clearInterval(interval))
</script>

<header>
  <div class="brand">
    <span class="logo">bonpy</span>
    <span class="tagline">Python sidecars · ML · AIOps</span>
  </div>
  <nav>
    <a href="/" title="bonsai UI (live network graph)">← bonsai UI</a>
  </nav>
</header>

<main>
  <StatusBanner {required_kinds} {missing_required} {sidecars} {last_fetch_ok} {last_error} />

  <section>
    <h2>Registered sidecars</h2>
    {#if sidecars.length === 0}
      <p class="empty">No sidecars registered. Start the rules sidecar with <code>python python/collector_engine.py</code>.</p>
    {:else}
      <div class="cards">
        {#each sidecars as s (s.sidecar_id)}
          <SidecarCard sidecar={s} />
        {/each}
      </div>
    {/if}
  </section>

  <section>
    <h2>Rule firing</h2>
    <RuleFiringTable {sidecars} {detections} />
  </section>

  <section>
    <h2>ML models</h2>
    <MlModelPanel {sidecars} />
  </section>
</main>

<footer>
  <span>CV7 T4-5 · read-only · editor / retraining / GNN console coming in CV8+</span>
  <span>See <code>docs/architecture/sidecars.md</code></span>
</footer>

<style>
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 1rem 1.5rem;
    border-bottom: 1px solid var(--border);
    background: var(--surface);
  }
  .brand { display: flex; align-items: baseline; gap: 0.75rem; }
  .logo {
    font-weight: 800;
    font-size: 1.4rem;
    color: var(--accent);
    letter-spacing: -0.02em;
  }
  .tagline { color: var(--text-dim); font-size: 0.85rem; }
  nav a { color: var(--text-dim); }
  nav a:hover { color: var(--accent); }
  main {
    max-width: 1200px;
    margin: 0 auto;
    padding: 1.5rem;
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
  }
  section {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 1.25rem;
  }
  section h2 { margin-bottom: 1rem; }
  .empty { color: var(--text-dim); }
  .empty code {
    background: var(--surface-2);
    padding: 2px 6px;
    border-radius: 3px;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(320px, 1fr));
    gap: 1rem;
  }
  footer {
    border-top: 1px solid var(--border);
    margin-top: 2rem;
    padding: 1rem 1.5rem;
    color: var(--text-dim);
    font-size: 0.8rem;
    display: flex;
    justify-content: space-between;
  }
</style>
