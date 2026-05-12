<!--
  Command palette (Ctrl+K / Cmd+K).
  Workspace shortcuts: Ctrl/Cmd + 1-9
  Mounts globally in App.svelte.
-->
<script>
  import { navigate } from '$lib/router.svelte.js';

  let open = $state(false);
  let query = $state('');
  let devices = $state([]);
  let sites = $state([]);
  let selectedIdx = $state(0);
  let inputEl = $state(null);

  // Cmd-1..9 workspace shortcuts (matches sidebar order)
  const WORKSPACE_SHORTCUTS = [
    { key: '1', label: 'Live',          route: '/' },
    { key: '2', label: 'Incidents',     route: '/incidents' },
    { key: '3', label: 'Devices',       route: '/devices' },
    { key: '4', label: 'Operations',    route: '/operations' },
    { key: '5', label: 'Collectors',    route: '/collectors' },
    { key: '6', label: 'Enrichment',    route: '/enrichment' },
    { key: '7', label: 'Adapters',      route: '/adapters' },
    { key: '8', label: 'Approvals',     route: '/approvals' },
    { key: '9', label: 'Explorer',      route: '/explorer' },
  ];

  const NAV_ITEMS = WORKSPACE_SHORTCUTS.map(ws => ({
    label: ws.label,
    icon: '◉',
    shortcut: ws.key,
    action: () => navigate(ws.route),
  })).concat([
    { label: 'Add Device',    icon: '+', action: () => navigate('/devices/new') },
    { label: 'Sites',         icon: '◎', action: () => navigate('/sites') },
    { label: 'Credentials',   icon: '⚿', action: () => navigate('/credentials') },
    { label: 'Investigations',icon: '🔍', action: () => navigate('/investigations') },
  ]);

  async function loadEntities() {
    try {
      const [devRes, siteRes] = await Promise.all([
        fetch('/api/onboarding/devices'),
        fetch('/api/sites'),
      ]);
      if (devRes.ok) {
        const d = await devRes.json();
        devices = (d.devices ?? []).map(dev => ({
          label: dev.hostname || dev.address,
          sub:   dev.address,
          icon:  '⊡',
          action: () => navigate(`/devices/${encodeURIComponent(dev.address)}`),
        }));
      }
      if (siteRes.ok) {
        const s = await siteRes.json();
        sites = (s.sites ?? []).map(site => ({
          label: site.name,
          sub:   site.kind || 'site',
          icon:  '◎',
          action: () => navigate('/sites'),
        }));
      }
    } catch { /* ignore */ }
  }

  const allItems = $derived([
    ...NAV_ITEMS.map(n => ({ ...n, kind: 'nav' })),
    ...devices.map(d => ({ ...d, kind: 'device' })),
    ...sites.map(s => ({ ...s, kind: 'site' })),
  ]);

  const filtered = $derived(
    query.trim()
      ? allItems.filter(item =>
          item.label.toLowerCase().includes(query.toLowerCase()) ||
          (item.sub ?? '').toLowerCase().includes(query.toLowerCase())
        )
      : allItems
  );

  $effect(() => { selectedIdx = 0; });

  function show() {
    if (!open) { open = true; loadEntities(); }
  }

  function hide() {
    open = false;
    query = '';
    selectedIdx = 0;
  }

  function run(item) {
    item.action();
    hide();
  }

  function onKeydown(e) {
    // Cmd/Ctrl + 1-9: direct workspace jump (no palette needed)
    if ((e.ctrlKey || e.metaKey) && e.key >= '1' && e.key <= '9') {
      e.preventDefault();
      const ws = WORKSPACE_SHORTCUTS[parseInt(e.key) - 1];
      if (ws) navigate(ws.route);
      if (open) hide();
      return;
    }

    // Cmd/Ctrl + K: toggle palette
    if ((e.ctrlKey || e.metaKey) && e.key === 'k') {
      e.preventDefault();
      open ? hide() : show();
      return;
    }

    if (!open) return;
    if (e.key === 'Escape') { hide(); return; }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      selectedIdx = Math.min(selectedIdx + 1, filtered.length - 1);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      selectedIdx = Math.max(selectedIdx - 1, 0);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (filtered[selectedIdx]) run(filtered[selectedIdx]);
    }
  }

  function onBackdropClick(e) {
    if (e.target === e.currentTarget) hide();
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="palette-backdrop" onclick={onBackdropClick} role="presentation">
    <div class="palette" role="dialog" aria-label="Command palette" aria-modal="true">

      <div class="palette-input-row">
        <span class="palette-icon">⌨</span>
        <input
          bind:this={inputEl}
          bind:value={query}
          placeholder="Go to workspace, device, site…"
          class="palette-input"
          aria-label="Search"
          autofocus
        />
        <kbd class="esc-hint">ESC</kbd>
      </div>

      <ul class="palette-list" role="listbox">
        {#each filtered.slice(0, 12) as item, i (item.label + (item.sub ?? ''))}
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <li
            class="palette-item"
            class:selected={i === selectedIdx}
            class:kind-device={item.kind === 'device'}
            class:kind-site={item.kind === 'site'}
            role="option"
            aria-selected={i === selectedIdx}
            onclick={() => run(item)}
          >
            <span class="item-icon">{item.icon}</span>
            <span class="item-text">
              <span class="item-label">{item.label}</span>
              {#if item.sub}<span class="item-sub">{item.sub}</span>{/if}
            </span>
            <div class="item-right">
              {#if item.shortcut}
                <kbd class="shortcut-hint">⌘{item.shortcut}</kbd>
              {/if}
              <span class="item-kind">{item.kind}</span>
            </div>
          </li>
        {/each}
        {#if filtered.length === 0}
          <li class="palette-empty">No results for "{query}"</li>
        {/if}
      </ul>

      <div class="palette-footer">
        <span><kbd>↑↓</kbd> navigate</span>
        <span><kbd>↵</kbd> select</span>
        <span><kbd>Esc</kbd> close</span>
        <span class="palette-hint"><kbd>⌘1-9</kbd> jump to workspace</span>
      </div>
    </div>
  </div>
{/if}

<style>
  .palette-backdrop {
    position: fixed; inset: 0;
    background: rgba(0,0,0,0.6);
    display: flex; align-items: flex-start; justify-content: center;
    padding-top: 100px;
    z-index: 1000;
    animation: fade-in var(--duration-fast) var(--ease-out);
  }

  @keyframes fade-in { from { opacity: 0; } to { opacity: 1; } }

  .palette {
    width: min(600px, 92vw);
    background: var(--bg-elevated);
    border: 1px solid var(--border-default);
    border-radius: 8px;
    box-shadow: 0 20px 60px rgba(0,0,0,0.7);
    overflow: hidden;
    animation: slide-in var(--duration-fast) var(--ease-out);
  }

  @keyframes slide-in { from { opacity: 0; transform: translateY(-10px); } to { opacity: 1; transform: none; } }

  .palette-input-row {
    display: flex; align-items: center; gap: 10px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border-subtle);
  }

  .palette-icon { color: var(--text-secondary); font-size: 15px; }

  .palette-input {
    flex: 1; background: none; border: none; outline: none;
    color: var(--text-primary);
    font-size: 15px;
    font-family: var(--font-sans);
    font-weight: 500;
  }

  .palette-input::placeholder { color: var(--text-tertiary); }

  .esc-hint {
    font-size: 10px; color: var(--text-tertiary);
    border: 1px solid var(--border-subtle);
    border-radius: 3px; padding: 1px 5px;
    font-family: var(--font-mono);
  }

  .palette-list {
    list-style: none; margin: 0; padding: 4px 0;
    max-height: 360px; overflow-y: auto;
  }

  .palette-item {
    display: flex; align-items: center; gap: 10px;
    padding: 8px 14px;
    cursor: pointer;
    font-size: 13px;
    color: var(--text-primary);
    transition: background var(--duration-instant) var(--ease-out);
  }

  .palette-item:hover,
  .palette-item.selected {
    background: var(--bg-glass);
  }

  .palette-item.selected {
    background: rgba(94,234,212,0.06);
  }

  .item-icon { width: 16px; text-align: center; color: var(--text-secondary); flex-shrink: 0; }
  .item-text { flex: 1; display: flex; flex-direction: column; gap: 1px; min-width: 0; }
  .item-label { font-weight: 500; }
  .item-sub { font-size: 11px; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .item-right {
    display: flex; align-items: center; gap: 6px; flex-shrink: 0;
  }

  .shortcut-hint {
    font-size: 10px; color: var(--text-tertiary);
    border: 1px solid var(--border-subtle);
    border-radius: 3px; padding: 1px 5px;
    font-family: var(--font-mono);
  }

  .item-kind {
    font-size: 10px; color: var(--text-tertiary);
    text-transform: uppercase; letter-spacing: 0.04em;
  }

  .palette-empty {
    padding: 16px; text-align: center;
    color: var(--text-secondary); font-size: 13px;
  }

  .palette-footer {
    display: flex; gap: 14px; padding: 7px 14px;
    border-top: 1px solid var(--border-subtle);
    font-size: 11px; color: var(--text-tertiary);
    background: var(--bg-surface);
  }

  .palette-hint { margin-left: auto; }

  kbd {
    border: 1px solid var(--border-subtle);
    border-radius: 3px;
    padding: 0 4px;
    font-size: 10px;
    font-family: var(--font-mono);
    color: var(--text-tertiary);
  }
</style>
