<script>
  import { onMount } from 'svelte';
  import { toast } from '$lib/toast.svelte.js';

  let activeTab = $state('synthesizer');
  let loading = $state(true);
  let items = $state([]);
  let filter = $state('');
  let selected = $state(null);
  let editing = $state(false);
  let editJson = $state('');
  let saving = $state(false);

  // Config class shown in each tab
  const TABS = [
    { key: 'synthesizer',       label: 'Detection Rules',   configClass: 'synthesizer_rules' },
    { key: 'gnmi_path',         label: 'gNMI Path Profiles', configClass: 'path_profiles' },
    { key: 'vendor_state',      label: 'Vendor State Maps',  configClass: 'vendor_state_mapping' },
    { key: 'gnmi_known_issues', label: 'Known Issues',       configClass: 'gnmi_known_issues' },
    { key: 'playbook',          label: 'Playbooks',          configClass: 'playbook' },
  ];

  function currentClass() {
    return TABS.find(t => t.key === activeTab)?.configClass ?? 'synthesizer_rules';
  }

  onMount(() => loadItems());

  async function loadItems() {
    loading = true;
    selected = null;
    editing = false;
    try {
      const cls = currentClass();
      const r = await fetch(`/api/config-items?class=${encodeURIComponent(cls)}`);
      if (!r.ok) throw new Error(await r.text());
      items = await r.json();
    } catch (e) {
      toast(e.message, 'error');
      items = [];
    } finally {
      loading = false;
    }
  }

  function switchTab(key) {
    activeTab = key;
    filter = '';
    loadItems();
  }

  let filtered = $derived(
    filter.trim()
      ? items.filter(i =>
          i.name.toLowerCase().includes(filter.toLowerCase()) ||
          i.vendor.toLowerCase().includes(filter.toLowerCase()) ||
          i.id.toLowerCase().includes(filter.toLowerCase())
        )
      : items
  );

  function selectItem(item) {
    selected = item;
    editing = false;
    editJson = '';
  }

  function startEdit() {
    if (!selected) return;
    try {
      const parsed = JSON.parse(selected.content_json || '{}');
      editJson = JSON.stringify(parsed, null, 2);
    } catch {
      editJson = selected.content_json || '';
    }
    editing = true;
  }

  function cancelEdit() {
    editing = false;
    editJson = '';
  }

  async function saveEdit() {
    if (!selected) return;
    saving = true;
    try {
      // Validate JSON
      JSON.parse(editJson);
      const payload = {
        ...selected,
        content_json: editJson,
      };
      const r = await fetch('/api/config-items', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      if (!r.ok) throw new Error(await r.text());
      toast('Saved.', 'success');
      editing = false;
      await loadItems();
      // Reselect the item
      selected = items.find(i => i.id === payload.id) ?? null;
    } catch (e) {
      toast(e.message, 'error');
    } finally {
      saving = false;
    }
  }

  async function toggleEnabled(item) {
    const payload = { ...item, enabled: !item.enabled };
    try {
      const r = await fetch('/api/config-items', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload),
      });
      if (!r.ok) throw new Error(await r.text());
      await loadItems();
      if (selected?.id === item.id) {
        selected = items.find(i => i.id === item.id) ?? null;
      }
    } catch (e) {
      toast(e.message, 'error');
    }
  }

  function prettyContent(json) {
    try {
      return JSON.stringify(JSON.parse(json), null, 2);
    } catch {
      return json || '';
    }
  }
</script>

<div class="workspace">
  <div class="workspace-header">
    <h1>Config Library</h1>
    <p class="muted">DB-backed configuration items. Edit in-place; changes take effect on next reload cycle.</p>
  </div>

  <div class="tabs">
    {#each TABS as tab}
      <button class="tab" class:active={activeTab === tab.key} onclick={() => switchTab(tab.key)}>
        {tab.label}
        {#if activeTab === tab.key && !loading}
          <span class="tab-count">{items.length}</span>
        {/if}
      </button>
    {/each}
  </div>

  <div class="split">
    <!-- Left: item list -->
    <div class="list-col">
      <div class="search-row">
        <input bind:value={filter} placeholder="Filter by name, vendor, or ID…" class="search-input" />
        <span class="count-label">
          {#if loading}Loading…{:else}{filtered.length} items{/if}
        </span>
      </div>

      {#if loading}
        <div class="empty-state">Loading…</div>
      {:else if filtered.length === 0}
        <div class="empty-state">No items match.</div>
      {:else}
        <ul class="item-list">
          {#each filtered as item}
            <li
              class="item-row"
              class:active={selected?.id === item.id}
              class:disabled={!item.enabled}
              onclick={() => selectItem(item)}
              role="button"
              tabindex="0"
              onkeydown={(e) => e.key === 'Enter' && selectItem(item)}
            >
              <div class="item-header">
                <span class="item-name">{item.name || item.id}</span>
                <button
                  class="toggle-btn"
                  class:on={item.enabled}
                  onclick={(e) => { e.stopPropagation(); toggleEnabled(item); }}
                  title={item.enabled ? 'Disable' : 'Enable'}
                >
                  {item.enabled ? '●' : '○'}
                </button>
              </div>
              <div class="item-meta">
                {#if item.vendor}<span class="badge vendor-badge">{item.vendor}</span>{/if}
                <span class="badge class-badge">{item.config_class}</span>
                {#if item.version}<span class="muted small">v{item.version}</span>{/if}
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <!-- Right: detail -->
    <div class="detail-col">
      {#if selected}
        <div class="detail-card">
          <div class="detail-header">
            <h2>{selected.name || selected.id}</h2>
            <span class="status-badge" class:enabled={selected.enabled}>{selected.enabled ? 'Enabled' : 'Disabled'}</span>
          </div>

          <div class="detail-meta">
            <div><strong>ID:</strong> <code>{selected.id}</code></div>
            <div><strong>Class:</strong> {selected.config_class}</div>
            {#if selected.vendor}<div><strong>Vendor:</strong> {selected.vendor}</div>{/if}
            {#if selected.version}<div><strong>Version:</strong> {selected.version}</div>{/if}
            {#if selected.created_by}<div><strong>Created by:</strong> {selected.created_by}</div>{/if}
          </div>

          {#if editing}
            <div class="edit-section">
              <label class="edit-label">Content JSON</label>
              <textarea bind:value={editJson} class="json-editor" rows="20" spellcheck="false"></textarea>
              <div class="edit-actions">
                <button class="primary-btn" onclick={saveEdit} disabled={saving}>
                  {saving ? 'Saving…' : 'Save'}
                </button>
                <button class="secondary-btn" onclick={cancelEdit}>Cancel</button>
              </div>
            </div>
          {:else}
            <div class="content-section">
              <div class="content-header">
                <span class="section-label">Content</span>
                <button class="ghost-btn" onclick={startEdit}>Edit</button>
              </div>
              <pre class="json-view">{prettyContent(selected.content_json)}</pre>
            </div>
          {/if}
        </div>
      {:else}
        <div class="empty-state">Select an item to view details.</div>
      {/if}
    </div>
  </div>
</div>

<style>
  .workspace { padding: 24px; max-width: 1200px; }
  .workspace-header { margin-bottom: 20px; }
  .workspace-header h1 { margin: 0 0 6px; font-size: 22px; font-weight: 600; }
  .workspace-header p { margin: 0; }
  .muted { color: var(--fg-muted, #888); }
  .small { font-size: 12px; }

  .tabs { display: flex; gap: 4px; margin-bottom: 20px; border-bottom: 1px solid var(--border); padding-bottom: 10px; flex-wrap: wrap; }
  .tab { background: transparent; border: none; color: var(--fg-muted, #888); font-size: 13px; cursor: pointer; padding: 6px 12px; border-radius: 4px; display: flex; align-items: center; gap: 6px; }
  .tab:hover { background: rgba(255,255,255,0.05); }
  .tab.active { color: var(--fg); font-weight: 600; background: rgba(255,255,255,0.1); }
  .tab-count { font-size: 10px; background: var(--border); padding: 1px 6px; border-radius: 10px; }

  .split { display: grid; grid-template-columns: 340px 1fr; gap: 20px; }
  @media (max-width: 800px) { .split { grid-template-columns: 1fr; } }

  .list-col { display: flex; flex-direction: column; gap: 8px; }
  .search-row { display: flex; align-items: center; gap: 10px; }
  .search-input { flex: 1; padding: 7px 10px; font-size: 13px; border: 1px solid var(--border); border-radius: 6px; background: var(--input-bg, #111); color: var(--fg); }
  .count-label { font-size: 12px; color: var(--fg-muted, #888); white-space: nowrap; }

  .item-list { list-style: none; padding: 0; margin: 0; display: flex; flex-direction: column; gap: 4px; max-height: 70vh; overflow-y: auto; }
  .item-row {
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
    transition: border-color 0.15s, background 0.15s;
  }
  .item-row:hover { border-color: var(--accent, #58a6ff); }
  .item-row.active { border-color: var(--accent, #58a6ff); background: rgba(88,166,255,0.06); }
  .item-row.disabled { opacity: 0.55; }

  .item-header { display: flex; align-items: center; justify-content: space-between; gap: 6px; }
  .item-name { font-size: 13px; font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .item-meta { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 4px; }

  .toggle-btn { background: none; border: none; cursor: pointer; font-size: 14px; padding: 0 4px; }
  .toggle-btn.on { color: #22c55e; }
  .toggle-btn:not(.on) { color: #6b7280; }

  .badge { font-size: 10px; padding: 1px 6px; border-radius: 10px; font-weight: 600; }
  .vendor-badge { background: rgba(180,120,255,0.15); color: #c47aff; }
  .class-badge { background: rgba(88,166,255,0.12); color: var(--accent, #58a6ff); }

  .detail-col { min-width: 0; }
  .detail-card { background: var(--card-bg, #1a1a2e); border: 1px solid var(--border); border-radius: 8px; padding: 20px; }
  .detail-header { display: flex; align-items: baseline; gap: 10px; margin-bottom: 12px; flex-wrap: wrap; }
  .detail-header h2 { margin: 0; font-size: 18px; word-break: break-all; }
  .status-badge { font-size: 10px; padding: 2px 8px; border-radius: 10px; font-weight: 700; text-transform: uppercase; }
  .status-badge.enabled { background: rgba(34,197,94,0.15); color: #22c55e; }
  .status-badge:not(.enabled) { background: rgba(107,114,128,0.2); color: #6b7280; }

  .detail-meta { font-size: 13px; line-height: 1.8; margin-bottom: 16px; }
  .detail-meta code { font-family: monospace; font-size: 12px; background: var(--border); padding: 1px 4px; border-radius: 3px; }

  .content-section { margin-top: 12px; }
  .content-header { display: flex; justify-content: space-between; align-items: center; margin-bottom: 8px; }
  .section-label { font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.06em; color: var(--fg-muted, #888); }

  .json-view {
    background: var(--bg-surface, #0d0d1a);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 12px;
    font-family: monospace;
    font-size: 12px;
    line-height: 1.5;
    overflow: auto;
    max-height: 50vh;
    white-space: pre-wrap;
    word-break: break-word;
    margin: 0;
    color: var(--fg);
  }

  .edit-section { margin-top: 12px; }
  .edit-label { font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.06em; color: var(--fg-muted, #888); display: block; margin-bottom: 6px; }
  .json-editor {
    width: 100%;
    box-sizing: border-box;
    background: var(--bg-surface, #0d0d1a);
    border: 1px solid var(--accent, #58a6ff);
    border-radius: 6px;
    padding: 12px;
    font-family: monospace;
    font-size: 12px;
    line-height: 1.5;
    color: var(--fg);
    resize: vertical;
  }
  .edit-actions { display: flex; gap: 8px; margin-top: 10px; }

  .primary-btn { background: var(--accent, #58a6ff); color: #000; border: none; padding: 7px 16px; border-radius: 6px; font-weight: 600; cursor: pointer; font-size: 13px; }
  .primary-btn:disabled { opacity: 0.5; cursor: not-allowed; }
  .secondary-btn { background: transparent; border: 1px solid var(--border); color: var(--fg); padding: 7px 16px; border-radius: 6px; cursor: pointer; font-size: 13px; }
  .ghost-btn { background: none; border: 1px solid var(--border); border-radius: 4px; color: var(--fg-muted); padding: 4px 10px; cursor: pointer; font-size: 11px; }
  .ghost-btn:hover { border-color: var(--accent); color: var(--fg); }

  .empty-state { color: var(--fg-muted, #888); font-size: 14px; padding: 32px 0; text-align: center; }
</style>
