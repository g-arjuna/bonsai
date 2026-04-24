<script>
  import { onMount } from 'svelte';
  import { toast } from '$lib/toast.svelte.js';
  import { relativeTime, absoluteTime } from '$lib/timeutil.js';

  let sites = $state([]);
  let environments = $state([]);
  let siteSummary = $state(null);
  let loading = $state(true);
  let selected = $state(null);
  let detailLoading = $state(false);

  let newName = $state('');
  let newParent = $state('');
  let newKind = $state('dc');
  let newEnvironmentId = $state('');
  let saving = $state(false);

  let editName = $state('');
  let editParent = $state('');
  let editKind = $state('');
  let editLat = $state('');
  let editLon = $state('');
  let editEnvironmentId = $state('');
  let editing = $state(false);
  let removing = $state(false);
  let assigningEnv = $state(false);

  const nameInputId = 'site-name';
  const kindInputId = 'site-kind';
  const parentInputId = 'site-parent';
  const KIND_OPTIONS = ['region', 'dc', 'pod', 'rack', 'other'];

  const ARCHETYPE_LABEL = {
    data_center: 'DC', campus_wired: 'Campus', campus_wireless: 'Wireless',
    service_provider: 'SP', home_lab: 'Lab',
  };

  onMount(() => { loadSites(); loadEnvironments(); });

  $effect(() => {
    if (selected) {
      editName = selected.name ?? '';
      editParent = selected.parent_id ?? '';
      editKind = selected.kind || 'other';
      editLat = selected.lat ? String(selected.lat) : '';
      editLon = selected.lon ? String(selected.lon) : '';
      editEnvironmentId = selected.environment_id ?? '';
      loadSiteSummary(selected.id);
    } else {
      siteSummary = null;
    }
  });

  async function loadSites() {
    loading = true;
    try {
      const r = await fetch('/api/sites');
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      sites = data.sites ?? [];
      if (selected) {
        selected = sites.find((site) => site.id === selected.id) ?? null;
      }
    } catch (e) {
      toast(e.message, 'error');
      sites = [];
    } finally {
      loading = false;
    }
  }

  async function loadEnvironments() {
    try {
      const r = await fetch('/api/environments');
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      environments = data.environments ?? [];
    } catch (e) {
      environments = [];
    }
  }

  async function assignEnvironment() {
    if (!selected) return;
    assigningEnv = true;
    try {
      const r = await fetch('/api/environments/assign-site', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ site_id: selected.id, environment_id: editEnvironmentId }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(`Environment assigned for "${selected.name}".`, 'success');
      await loadSites();
      selected = sites.find(s => s.id === selected.id) ?? null;
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      assigningEnv = false;
    }
  }

  async function loadSiteSummary(id) {
    detailLoading = true;
    try {
      const r = await fetch(`/api/sites/${encodeURIComponent(id)}`);
      if (!r.ok) throw new Error(await r.text());
      siteSummary = await r.json();
    } catch (e) {
      toast(e.message, 'error');
      siteSummary = null;
    } finally {
      detailLoading = false;
    }
  }

  async function addSite() {
    if (!newName.trim()) return;
    saving = true;
    try {
      const r = await fetch('/api/sites', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          name: newName.trim(),
          parent_id: newParent.trim(),
          kind: newKind,
          lat: 0,
          lon: 0,
          metadata_json: '{}',
          environment_id: newEnvironmentId,
        }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(`Site "${newName.trim()}" created.`, 'success');
      newName = '';
      newParent = '';
      newKind = 'dc';
      newEnvironmentId = '';
      await loadSites();
      selected = data.site ?? null;
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      saving = false;
    }
  }

  async function saveSelectedSite() {
    if (!selected || !editName.trim()) return;
    editing = true;
    try {
      const r = await fetch('/api/sites', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          id: selected.id,
          name: editName.trim(),
          parent_id: editParent,
          kind: editKind || 'other',
          lat: editLat ? Number(editLat) : 0,
          lon: editLon ? Number(editLon) : 0,
          metadata_json: selected.metadata_json || '{}',
          environment_id: editEnvironmentId,
        }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(`Updated site "${editName.trim()}".`, 'success');
      await loadSites();
      selected = data.site ?? selected;
      await loadSiteSummary(selected.id);
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      editing = false;
    }
  }

  async function deleteSelectedSite() {
    if (!selected) return;
    removing = true;
    try {
      const r = await fetch('/api/sites/remove', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id: selected.id }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(`Removed site "${selected.name}".`, 'success');
      selected = null;
      siteSummary = null;
      await loadSites();
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      removing = false;
    }
  }

  function buildTree(flat) {
    const byId = new Map(flat.map((site) => [site.id, { ...site, children: [] }]));
    const roots = [];
    for (const site of byId.values()) {
      if (site.parent_id && byId.has(site.parent_id)) {
        byId.get(site.parent_id).children.push(site);
      } else {
        roots.push(site);
      }
    }
    return roots;
  }

  function selectableParents(currentId) {
    return sites.filter((site) => site.id !== currentId);
  }

  let tree = $derived(buildTree(sites));
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Topology</p>
      <h2>Sites</h2>
    </div>
  </div>

  <div class="sites-layout">
    <div class="sites-tree-col">
      <div class="card">
        <h3 style="margin-bottom: 12px; font-size: 15px;">Site hierarchy</h3>
        {#if loading}
          <div class="muted">Loading…</div>
        {:else if sites.length === 0}
          <div class="empty">No sites yet. Add one below.</div>
        {:else}
          <div class="site-tree">
            {#each tree as root}
              <SiteNode node={root} depth={0} bind:selected />
            {/each}
          </div>
        {/if}
      </div>

      <div class="card" style="margin-top: 12px;">
        <h3 style="margin-bottom: 12px; font-size: 15px;">Add site</h3>
        <div class="add-form">
          <div class="form-row">
            <label for={nameInputId}>Name</label>
            <input id={nameInputId} bind:value={newName} placeholder="e.g. dc-london" autocomplete="off" />
          </div>
          <div class="form-row">
            <label for={kindInputId}>Kind</label>
            <select id={kindInputId} bind:value={newKind}>
              {#each KIND_OPTIONS as kind}
                <option value={kind}>{kind}</option>
              {/each}
            </select>
          </div>
          <div class="form-row">
            <label for={parentInputId}>Parent site</label>
            <select id={parentInputId} bind:value={newParent}>
              <option value="">Root</option>
              {#each sites as site}
                <option value={site.id}>{site.name}</option>
              {/each}
            </select>
          </div>
          <div class="form-row">
            <label for="site-env">Environment</label>
            <select id="site-env" bind:value={newEnvironmentId}>
              <option value="">None</option>
              {#each environments as env}
                <option value={env.id}>{env.name} ({ARCHETYPE_LABEL[env.archetype] ?? env.archetype})</option>
              {/each}
            </select>
          </div>
          <button onclick={addSite} disabled={saving || !newName.trim()}>
            {saving ? 'Saving…' : 'Add site'}
          </button>
        </div>
      </div>
    </div>

    <div class="site-detail-col">
      {#if !selected}
        <div class="empty" style="margin-top: 0;">Select a site to inspect its devices, health, and recent detections.</div>
      {:else}
        <div class="card">
          <div class="detail-header">
            <div>
              <h3 style="margin-bottom: 6px; font-size: 18px;">{selected.name}</h3>
              <div class="muted small">ID <code>{selected.id}</code></div>
            </div>
            <button class="danger" onclick={deleteSelectedSite} disabled={removing}>
              {removing ? 'Removing…' : 'Delete'}
            </button>
          </div>

          <div class="detail-grid" style="margin-top: 12px;">
            <span class="muted">Name</span>
            <input bind:value={editName} autocomplete="off" />

            <span class="muted">Kind</span>
            <select bind:value={editKind}>
              {#each KIND_OPTIONS as kind}
                <option value={kind}>{kind}</option>
              {/each}
            </select>

            <span class="muted">Parent</span>
            <select bind:value={editParent}>
              <option value="">Root</option>
              {#each selectableParents(selected.id) as site}
                <option value={site.id}>{site.name}</option>
              {/each}
            </select>

            <span class="muted">Latitude</span>
            <input bind:value={editLat} placeholder="optional" autocomplete="off" />

            <span class="muted">Longitude</span>
            <input bind:value={editLon} placeholder="optional" autocomplete="off" />

            <span class="muted">Environment</span>
            <div class="env-assign-row">
              <select bind:value={editEnvironmentId}>
                <option value="">None</option>
                {#each environments as env}
                  <option value={env.id}>{env.name} ({ARCHETYPE_LABEL[env.archetype] ?? env.archetype})</option>
                {/each}
              </select>
              <button
                class="btn-small"
                onclick={assignEnvironment}
                disabled={assigningEnv || editEnvironmentId === (selected?.environment_id ?? '')}
              >
                {assigningEnv ? '…' : 'Assign'}
              </button>
            </div>
          </div>

          <button style="margin-top: 12px;" onclick={saveSelectedSite} disabled={editing || !editName.trim()}>
            {editing ? 'Saving…' : 'Save site'}
          </button>
        </div>

        {#if detailLoading}
          <div class="card" style="margin-top: 12px;"><div class="muted">Loading site detail…</div></div>
        {:else if siteSummary}
          <div class="summary-grid" style="margin-top: 12px;">
            <div class="card metric">
              <span>Devices</span>
              <strong>{siteSummary.device_count ?? 0}</strong>
            </div>
            <div class="card metric">
              <span>Child sites</span>
              <strong>{siteSummary.child_site_count ?? 0}</strong>
            </div>
            <div class="card metric">
              <span>Healthy / warn / critical</span>
              <strong>{siteSummary.health?.healthy ?? 0} / {siteSummary.health?.warn ?? 0} / {siteSummary.health?.critical ?? 0}</strong>
            </div>
            <div class="card metric">
              <span>Subscriptions</span>
              <strong>{siteSummary.subscription_summary?.observed ?? 0} obs</strong>
            </div>
          </div>

          <div class="card" style="margin-top: 12px;">
            <h3 style="margin-bottom: 12px; font-size: 15px;">Devices in site subtree</h3>
            {#if (siteSummary.devices?.length ?? 0) === 0}
              <div class="empty">No devices assigned to this site yet.</div>
            {:else}
              <table>
                <thead>
                  <tr>
                    <th>Device</th>
                    <th>Role</th>
                    <th>Collector</th>
                    <th>Health</th>
                  </tr>
                </thead>
                <tbody>
                  {#each siteSummary.devices as device}
                    <tr>
                      <td>
                        <strong>{device.hostname || device.address}</strong><br />
                        <span class="muted small">{device.address}</span>
                      </td>
                      <td>{device.role || '—'}</td>
                      <td>{device.collector_id || 'unassigned'}</td>
                      <td><span class="badge {device.health === 'healthy' ? 'healthy' : device.health === 'warn' ? 'warn' : 'critical'}">{device.health}</span></td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}
          </div>

          <div class="card" style="margin-top: 12px;">
            <h3 style="margin-bottom: 12px; font-size: 15px;">Recent detections</h3>
            {#if (siteSummary.recent_detections?.length ?? 0) === 0}
              <div class="empty">No recent detections in this site subtree.</div>
            {:else}
              <div class="event-list">
                {#each siteSummary.recent_detections as det}
                  <div class="event-row">
                    <span class="ts" title={absoluteTime(det.fired_at_ns)}>{relativeTime(det.fired_at_ns)}</span>
                    <div class="body">
                      <span class="badge {det.severity === 'critical' ? 'critical' : det.severity === 'high' ? 'warn' : 'info'}">{det.severity}</span>
                      <strong style="margin-left: 8px;">{det.device_address}</strong>
                      <span class="muted" style="display:block; margin-top: 4px;">{det.rule_id || 'detection'}</span>
                    </div>
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        {/if}
      {/if}
    </div>
  </div>
</div>

{#snippet SiteNode({ node, depth })}
  <div class="site-node" style="padding-left: {depth * 16}px;">
    <button class="site-row" class:active={selected?.id === node.id} onclick={() => (selected = node)}>
      <span class="kind-dot kind-{node.kind || 'other'}"></span>
      <span class="site-name">{node.name}</span>
      {#if node.kind}<span class="site-kind">{node.kind}</span>{/if}
      {#if node.environment_id}
        {@const envName = environments.find(e => e.id === node.environment_id)?.name}
        {#if envName}<span class="env-tag">{envName}</span>{/if}
      {/if}
    </button>
    {#if node.children?.length}
      {#each node.children as child}
        {@render SiteNode({ node: child, depth: depth + 1 })}
      {/each}
    {/if}
  </div>
{/snippet}

<style>
  .sites-layout { display: grid; grid-template-columns: 320px 1fr; gap: 16px; align-items: start; }
  .site-tree { display: grid; gap: 2px; }
  .site-node { display: flex; flex-direction: column; gap: 2px; }
  .site-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px;
    border-radius: 5px;
    border: none;
    background: transparent;
    color: var(--text);
    cursor: pointer;
    text-align: left;
    font-size: 13px;
    font-family: inherit;
    width: 100%;
  }
  .site-row:hover { background: rgba(255,255,255,0.05); }
  .site-row.active { background: rgba(88,166,255,0.12); color: var(--blue); }
  .kind-dot { width: 8px; height: 8px; border-radius: 50%; flex-shrink: 0; background: var(--muted); }
  .kind-dot.kind-region { background: var(--blue); }
  .kind-dot.kind-dc { background: var(--green); }
  .kind-dot.kind-pod { background: var(--yellow); }
  .site-name { flex: 1; }
  .site-kind { font-size: 11px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.05em; }
  .env-tag { font-size: 10px; background: rgba(88,166,255,0.15); color: var(--blue, #58a6ff); border-radius: 3px; padding: 1px 5px; }
  .add-form { display: grid; gap: 10px; }
  .detail-header { display: flex; justify-content: space-between; gap: 12px; align-items: start; }
  .detail-grid { display: grid; grid-template-columns: 90px 1fr; gap: 8px 12px; font-size: 13px; }
  .env-assign-row { display: flex; gap: 6px; align-items: center; }
  .env-assign-row select { flex: 1; }
  .btn-small { padding: 4px 10px; font-size: 12px; white-space: nowrap; }
  .summary-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 12px; }
  .small { font-size: 12px; }
  .event-list { display: grid; gap: 10px; }
  .event-row { display: flex; gap: 10px; }
  .ts { min-width: 70px; font-size: 12px; color: var(--muted); }

  @media (max-width: 860px) {
    .sites-layout { grid-template-columns: 1fr; }
    .detail-grid { grid-template-columns: 1fr; }
  }
</style>
