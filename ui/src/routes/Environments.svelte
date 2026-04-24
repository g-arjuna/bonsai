<script>
  import { onMount } from 'svelte';
  import { toast } from '$lib/toast.svelte.js';

  const ARCHETYPES = [
    { value: 'data_center',        label: 'Data Center' },
    { value: 'campus_wired',       label: 'Campus Wired' },
    { value: 'campus_wireless',    label: 'Campus Wireless' },
    { value: 'service_provider',   label: 'Service Provider' },
    { value: 'home_lab',           label: 'Home Lab' },
  ];

  const ARCHETYPE_BADGE = {
    data_center:        'DC',
    campus_wired:       'Campus',
    campus_wireless:    'Wireless',
    service_provider:   'SP',
    home_lab:           'Lab',
  };

  let environments = $state([]);
  let loading     = $state(true);
  let selected    = $state(null);

  let newName      = $state('');
  let newArchetype = $state('home_lab');
  let saving       = $state(false);

  let editName      = $state('');
  let editArchetype = $state('');
  let editMeta      = $state('');
  let editing       = $state(false);
  let removing      = $state(false);

  onMount(loadEnvironments);

  $effect(() => {
    if (selected) {
      editName      = selected.name ?? '';
      editArchetype = selected.archetype ?? 'home_lab';
      editMeta      = selected.metadata_json ?? '{}';
    }
  });

  async function loadEnvironments() {
    loading = true;
    try {
      const r = await fetch('/api/environments');
      if (!r.ok) throw new Error(await r.text());
      const data = await r.json();
      environments = data.environments ?? [];
      if (selected) {
        selected = environments.find(e => e.id === selected.id) ?? null;
      }
    } catch (e) {
      toast(e.message, 'error');
      environments = [];
    } finally {
      loading = false;
    }
  }

  async function createEnvironment() {
    if (!newName.trim()) return;
    saving = true;
    try {
      const r = await fetch('/api/environments', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: newName.trim(), archetype: newArchetype }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(`Environment "${newName.trim()}" created.`, 'success');
      newName = '';
      await loadEnvironments();
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      saving = false;
    }
  }

  async function saveSelectedEnvironment() {
    if (!selected || !editName.trim()) return;
    editing = true;
    try {
      const r = await fetch('/api/environments/update', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          id: selected.id,
          name: editName.trim(),
          archetype: editArchetype,
          metadata_json: editMeta || '{}',
        }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(`Updated environment "${editName.trim()}".`, 'success');
      await loadEnvironments();
      selected = environments.find(e => e.id === selected.id) ?? null;
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      editing = false;
    }
  }

  async function deleteSelectedEnvironment() {
    if (!selected) return;
    removing = true;
    try {
      const r = await fetch('/api/environments/remove', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id: selected.id }),
      });
      const data = await r.json();
      if (!data.success) throw new Error(data.error);
      toast(`Removed environment "${selected.name}".`, 'success');
      selected = null;
      await loadEnvironments();
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      removing = false;
    }
  }

  function archetypeLabel(value) {
    return ARCHETYPES.find(a => a.value === value)?.label ?? value;
  }
</script>

<div class="view">
  <div class="workspace-header">
    <div>
      <p class="eyebrow">Topology</p>
      <h2>Environments</h2>
    </div>
  </div>

  <div class="env-layout">
    <div class="env-list-col">
      <div class="card">
        <h3 style="margin-bottom: 12px; font-size: 15px;">Environments</h3>
        {#if loading}
          <div class="muted">Loading…</div>
        {:else if environments.length === 0}
          <div class="empty">No environments yet. Create one below.</div>
        {:else}
          <div class="env-list">
            {#each environments as env}
              <button
                class="env-row"
                class:active={selected?.id === env.id}
                onclick={() => (selected = env)}
              >
                <div class="env-row-main">
                  <span class="env-name">{env.name}</span>
                  <span class="badge archetype-badge">{ARCHETYPE_BADGE[env.archetype] ?? env.archetype}</span>
                </div>
                <div class="env-row-meta muted small">
                  {env.site_count} {env.site_count === 1 ? 'site' : 'sites'} &middot;
                  {env.device_count} {env.device_count === 1 ? 'device' : 'devices'}
                </div>
              </button>
            {/each}
          </div>
        {/if}
      </div>

      <div class="card" style="margin-top: 12px;">
        <h3 style="margin-bottom: 12px; font-size: 15px;">Create environment</h3>
        <div class="add-form">
          <div class="form-row">
            <label for="env-name">Name</label>
            <input id="env-name" bind:value={newName} placeholder="e.g. Lab DC Fabric" autocomplete="off" />
          </div>
          <div class="form-row">
            <label for="env-arch">Archetype</label>
            <select id="env-arch" bind:value={newArchetype}>
              {#each ARCHETYPES as a}
                <option value={a.value}>{a.label}</option>
              {/each}
            </select>
          </div>
          <button onclick={createEnvironment} disabled={saving || !newName.trim()}>
            {saving ? 'Creating…' : 'Create'}
          </button>
        </div>
      </div>
    </div>

    <div class="env-detail-col">
      {#if !selected}
        <div class="empty" style="margin-top: 0;">
          Select an environment to view and edit its configuration.
        </div>
      {:else}
        <div class="card">
          <div class="detail-header">
            <div>
              <h3 style="margin-bottom: 6px; font-size: 18px;">{selected.name}</h3>
              <div class="muted small">ID <code>{selected.id}</code></div>
            </div>
            <button
              class="danger"
              onclick={deleteSelectedEnvironment}
              disabled={removing}
            >
              {removing ? 'Removing…' : 'Delete'}
            </button>
          </div>

          <div class="detail-grid" style="margin-top: 12px;">
            <span class="muted">Name</span>
            <input bind:value={editName} autocomplete="off" />

            <span class="muted">Archetype</span>
            <select bind:value={editArchetype}>
              {#each ARCHETYPES as a}
                <option value={a.value}>{a.label}</option>
              {/each}
            </select>
          </div>

          <button
            style="margin-top: 12px;"
            onclick={saveSelectedEnvironment}
            disabled={editing || !editName.trim()}
          >
            {editing ? 'Saving…' : 'Save'}
          </button>
        </div>

        <div class="card" style="margin-top: 12px;">
          <div class="summary-row">
            <div class="metric">
              <span class="muted small">Sites</span>
              <strong>{selected.site_count}</strong>
            </div>
            <div class="metric">
              <span class="muted small">Devices</span>
              <strong>{selected.device_count}</strong>
            </div>
            <div class="metric">
              <span class="muted small">Archetype</span>
              <strong>{archetypeLabel(selected.archetype)}</strong>
            </div>
          </div>
          <p class="muted small" style="margin-top: 12px;">
            Sites are assigned to this environment from the
            <strong>Sites</strong> workspace or during device onboarding.
          </p>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .env-layout { display: grid; grid-template-columns: 300px 1fr; gap: 16px; align-items: start; }
  .env-list { display: grid; gap: 4px; }
  .env-row {
    display: block; width: 100%; text-align: left; padding: 10px 12px;
    border: 1px solid var(--border); border-radius: 6px; background: var(--bg);
    cursor: pointer; transition: background 0.1s;
  }
  .env-row:hover { background: var(--bg-hover, #f5f5f5); }
  .env-row.active { border-color: var(--accent, #2563eb); background: var(--accent-muted, #eff6ff); }
  .env-row-main { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .env-name { font-weight: 500; font-size: 14px; }
  .env-row-meta { margin-top: 4px; }
  .archetype-badge { background: var(--bg-code, #f0f0f0); color: var(--fg-muted, #555); font-size: 11px; padding: 1px 6px; border-radius: 4px; }
  .add-form { display: grid; gap: 10px; }
  .form-row { display: grid; grid-template-columns: 90px 1fr; align-items: center; gap: 8px; }
  .detail-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; }
  .detail-grid { display: grid; grid-template-columns: 100px 1fr; align-items: center; gap: 8px 12px; }
  .summary-row { display: flex; gap: 24px; }
  .metric { display: flex; flex-direction: column; gap: 4px; }
</style>
