<!--
  Command palette (Ctrl+K / Cmd+K).
  Searches devices, sites, and has navigation shortcuts.
  Props: none. Mounts globally in App.svelte.
-->
<script>
  import { navigate } from '$lib/router.svelte.js';

  let open = $state(false);
  let query = $state('');
  let devices = $state([]);
  let sites = $state([]);
  let selectedIdx = $state(0);
  let inputEl = $state(null);

  // Static nav shortcuts always available
  const NAV_ITEMS = [
    { label: 'Live / Topology',   icon: '◉', action: () => navigate('/') },
    { label: 'Incidents',         icon: '⚠', action: () => navigate('/incidents') },
    { label: 'Devices',           icon: '⊡', action: () => navigate('/devices') },
    { label: 'Add Device',        icon: '+', action: () => navigate('/devices/new') },
    { label: 'Collectors',        icon: '⇄', action: () => navigate('/collectors') },
    { label: 'Sites',             icon: '◎', action: () => navigate('/sites') },
    { label: 'Credentials',       icon: '⚿', action: () => navigate('/credentials') },
    { label: 'Operations',        icon: '♡', action: () => navigate('/operations') },
  ];

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

  $effect(() => {
    selectedIdx = 0;
  });

  function show() {
    if (!open) {
      open = true;
      loadEntities();
    }
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
          placeholder="Go to page, device, site…"
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
            class="palette-item {i === selectedIdx ? 'selected' : ''}"
            class:kind-device={item.kind === 'device'}
            class:kind-site={item.kind === 'site'}
            role="option"
            aria-selected={i === selectedIdx}
            onclick={() => run(item)}
          >
            <span class="item-icon">{item.icon}</span>
            <span class="item-text">
              <span class="item-label">{item.label}</span>
              {#if item.sub}
                <span class="item-sub">{item.sub}</span>
              {/if}
            </span>
            <span class="item-kind">{item.kind}</span>
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
        <span class="palette-hint">Ctrl+K to open</span>
      </div>
    </div>
  </div>
{/if}

<style>
  .palette-backdrop {
    position: fixed; inset: 0;
    background: rgba(0,0,0,0.55);
    display: flex; align-items: flex-start; justify-content: center;
    padding-top: 120px;
    z-index: 1000;
  }
  .palette {
    width: min(620px, 92vw);
    background: var(--bg1, #161b22);
    border: 1px solid var(--border, #30363d);
    border-radius: 8px;
    box-shadow: 0 16px 48px rgba(0,0,0,0.6);
    overflow: hidden;
  }
  .palette-input-row {
    display: flex; align-items: center; gap: 10px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border, #30363d);
  }
  .palette-icon { color: var(--muted, #8b949e); font-size: 16px; }
  .palette-input {
    flex: 1; background: none; border: none; outline: none;
    color: var(--text, #e6edf3); font-size: 15px;
  }
  .esc-hint {
    font-size: 10px; color: var(--muted); border: 1px solid var(--border);
    border-radius: 3px; padding: 1px 5px;
  }

  .palette-list { list-style: none; margin: 0; padding: 6px 0; max-height: 380px; overflow-y: auto; }

  .palette-item {
    display: flex; align-items: center; gap: 10px;
    padding: 9px 16px;
    cursor: pointer;
    font-size: 13px;
    color: var(--text, #e6edf3);
  }
  .palette-item:hover, .palette-item.selected { background: var(--bg2, #21262d); }
  .item-icon { width: 18px; text-align: center; color: var(--muted); }
  .item-text { flex: 1; display: flex; flex-direction: column; gap: 1px; }
  .item-label { font-weight: 500; }
  .item-sub { font-size: 11px; color: var(--muted, #8b949e); }
  .item-kind { font-size: 10px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; }

  .palette-empty { padding: 16px; text-align: center; color: var(--muted); font-size: 13px; }

  .palette-footer {
    display: flex; gap: 16px; padding: 8px 16px;
    border-top: 1px solid var(--border); font-size: 11px; color: var(--muted);
  }
  .palette-hint { margin-left: auto; }
  kbd {
    border: 1px solid var(--border); border-radius: 3px;
    padding: 0 4px; font-size: 10px;
  }
</style>
