<script>
  import { onMount, createEventDispatcher } from 'svelte';

  const dispatch = createEventDispatcher();

  const SOURCE_COLORS = {
    gnmi:      '#4dd0c8',
    syslog:    '#a78bfa',
    snmp:      '#f59e0b',
    netflow:   '#3b82f6',
    otlp:      '#10b981',
    bmp:       '#f97316',
    bgp_ls:    '#ec4899',
    detection: '#ef4444',
    registry:  '#6b7280',
  };
  const SOURCE_GROUPS = ['ALL', ...Object.keys(SOURCE_COLORS)];

  // Severity derived from event_type — no DC-specific assumptions.
  function eventSeverity(eventType) {
    const t = (eventType || '').toLowerCase();
    if (t.includes('down') || t.includes('fail') || t.includes('lost') || t.includes('critical') || t.includes('error')) return 'critical';
    if (t.includes('warn') || t.includes('flap') || t.includes('degrad') || t.includes('change') || t.includes('miss')) return 'warn';
    if (t === 'detection') return 'critical';
    return 'info';
  }

  const SEV_COLOR = { critical: '#f87171', warn: '#fbbf24', info: '#9ca3af' };
  const SEV_BG    = { critical: 'rgba(248,113,113,0.06)', warn: 'rgba(251,191,36,0.05)', info: 'transparent' };

  let allEvents    = $state([]);
  let paused       = $state(false);
  let activeSource = $state('ALL');
  let deviceFilter = $state('');
  let expandedIds  = $state(new Set());
  let sseConnected = $state(false);

  // Expose SSE state to parent (Live.svelte uses this for the status bar)
  const { onSseChange } = $props();

  function fmt(ns) {
    if (!ns) return '—';
    const d = new Date(ns / 1e6);
    const hh = d.getHours().toString().padStart(2,'0');
    const mm = d.getMinutes().toString().padStart(2,'0');
    const ss = d.getSeconds().toString().padStart(2,'0');
    return `${hh}:${mm}:${ss}`;
  }

  function srcColor(src) { return SOURCE_COLORS[src] ?? '#6b7280'; }

  function matchesFilters(ev) {
    if (activeSource !== 'ALL' && ev.source_type !== activeSource) return false;
    if (deviceFilter && !(ev.device_address ?? '').includes(deviceFilter)) return false;
    return true;
  }

  const visibleEvents = $derived(allEvents.filter(matchesFilters));

  function toggleExpand(id) {
    const s = new Set(expandedIds);
    s.has(id) ? s.delete(id) : s.add(id);
    expandedIds = s;
  }

  function parseDetail(ev) {
    if (!ev.detail_json) return null;
    try { return JSON.parse(ev.detail_json); } catch { return ev.detail_json; }
  }

  function fmtDetail(ev) {
    const d = parseDetail(ev);
    if (d === null) return '';
    if (typeof d === 'string') return d;
    if (ev.event_type === 'config_change_event') {
      const parts = [];
      if (d.yang_path) parts.push(d.yang_path);
      if (d.new_value !== undefined) parts.push(`→ ${JSON.stringify(d.new_value)}`);
      if (d.previous_value !== undefined && d.previous_value !== null) parts.push(`(was ${JSON.stringify(d.previous_value)})`);
      return parts.join(' ');
    }
    // For structured dicts: show key=value summary on one line
    const entries = Object.entries(d).slice(0, 5);
    return entries.map(([k,v]) => `${k}=${JSON.stringify(v)}`).join('  ');
  }

  async function loadHistory() {
    try {
      const params = new URLSearchParams({ limit: '150' });
      if (activeSource !== 'ALL') params.set('source', activeSource);
      if (deviceFilter) params.set('device', deviceFilter);
      const r = await fetch(`/api/events/history?${params}`);
      if (!r.ok) return;
      const data = await r.json();
      allEvents = data.events ?? [];
    } catch {}
  }

  // SSE with exponential-backoff reconnect
  let es = null;
  let reconnectTimer = null;
  let reconnectDelay = 1000;

  function connectSSE() {
    if (es) { es.close(); es = null; }
    es = new EventSource('/api/events');
    es.onopen = () => {
      sseConnected = true;
      reconnectDelay = 1000;
      onSseChange?.(true);
    };
    es.onmessage = (e) => {
      if (paused) return;
      try {
        const ev = JSON.parse(e.data);
        allEvents = [ev, ...allEvents].slice(0, 1000);
      } catch {}
    };
    es.onerror = () => {
      sseConnected = false;
      onSseChange?.(false);
      es.close(); es = null;
      reconnectTimer = setTimeout(() => {
        reconnectDelay = Math.min(reconnectDelay * 2, 30000);
        connectSSE();
      }, reconnectDelay);
    };
  }

  onMount(() => {
    loadHistory();
    connectSSE();
    return () => {
      es?.close();
      clearTimeout(reconnectTimer);
    };
  });
</script>

<div class="feed-shell">
  <!-- Header bar -->
  <div class="feed-header">
    <div class="feed-title">
      <span class="sse-dot" class:on={sseConnected}></span>
      <span class="title-text">Event Feed</span>
      <span class="count">{visibleEvents.length}</span>
    </div>
    <div class="feed-actions">
      <button class="act-btn" class:paused onclick={() => paused = !paused}
              title={paused ? 'Resume live feed' : 'Pause live feed'}>
        {paused ? '▶' : '⏸'}
      </button>
      <button class="act-btn" onclick={() => { allEvents = []; loadHistory(); }} title="Clear and reload">✕</button>
    </div>
  </div>

  <!-- Source filter chips -->
  <div class="source-bar">
    {#each SOURCE_GROUPS as src}
      {@const color = srcColor(src)}
      <button
        class="src-chip"
        class:active={activeSource === src}
        style={activeSource === src && src !== 'ALL'
          ? `background:${color}18; border-color:${color}; color:${color};`
          : ''}
        onclick={() => activeSource = src}
      >{src}</button>
    {/each}
  </div>

  <!-- Device search -->
  <div class="search-row">
    <input
      class="dev-search"
      type="text"
      placeholder="Filter by device…"
      bind:value={deviceFilter}
      oninput={loadHistory}
    />
  </div>

  <!-- Event list — fills remaining height -->
  <div class="event-list" role="feed" aria-label="Live events">
    {#if !visibleEvents.length}
      <div class="empty-state">
        {paused ? 'Feed paused.' : activeSource !== 'ALL' ? `No ${activeSource} events yet.` : 'Waiting for events…'}
      </div>
    {:else}
      {#each visibleEvents as ev (ev.id ?? ev.state_change_event_id ?? (ev.occurred_at_ns + '' + ev.event_type))}
        {@const traceId  = ev.state_change_event_id || ev.id || ''}
        {@const sev      = eventSeverity(ev.event_type)}
        {@const evKey    = ev.id ?? ev.state_change_event_id ?? (ev.occurred_at_ns + ev.event_type)}
        {@const expanded = expandedIds.has(evKey)}
        {@const detail   = fmtDetail(ev)}
        <div class="ev-row" style="background:{SEV_BG[sev]}">
          <div class="ev-main">
            <span class="ev-ts">{fmt(ev.occurred_at_ns)}</span>
            <span class="src-dot" style="background:{srcColor(ev.source_type ?? '')}" title={ev.source_type}></span>
            <span class="ev-type" style="color:{SEV_COLOR[sev]}">{ev.event_type}</span>
            <span class="ev-dev">{ev.device_address}</span>
            <div class="ev-actions">
              {#if traceId}
                <button class="act-mini" onclick={() => dispatch('trace', traceId)} title="View trace">⊹</button>
              {/if}
              {#if detail}
                <button class="act-mini" onclick={() => toggleExpand(evKey)} title="Toggle detail">
                  {expanded ? '▴' : '▾'}
                </button>
              {/if}
            </div>
          </div>
          {#if expanded && detail}
            <div class="ev-detail">
              {#if ev.event_type === 'config_change_event'}
                <span class="config-text">{detail}</span>
              {:else}
                <code>{detail}</code>
              {/if}
            </div>
          {/if}
        </div>
      {/each}
    {/if}
  </div>
</div>

<style>
  .feed-shell {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  /* ── Header ─────────────────────────────────────────────────────────────── */
  .feed-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 12px;
    border-bottom: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    flex-shrink: 0;
  }
  .feed-title { display: flex; align-items: center; gap: 7px; }
  .title-text { font-size: 13px; font-weight: 600; color: var(--text-primary, #e8eaed); }
  .count {
    font-size: 11px;
    color: var(--text-tertiary, #5f6368);
    background: rgba(255,255,255,0.05);
    padding: 1px 6px;
    border-radius: 10px;
  }

  .sse-dot {
    width: 7px; height: 7px; border-radius: 50%;
    background: var(--text-tertiary, #5f6368);
    flex-shrink: 0;
    transition: background 0.3s;
  }
  .sse-dot.on {
    background: #34d399;
    box-shadow: 0 0 5px rgba(52,211,153,0.45);
    animation: blink 2s ease-in-out infinite;
  }
  @keyframes blink { 0%,100%{opacity:1} 50%{opacity:0.45} }

  .feed-actions { display: flex; gap: 4px; }
  .act-btn {
    background: none; border: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    color: var(--text-secondary, #9aa0a6); padding: 3px 8px; border-radius: 4px;
    cursor: pointer; font-size: 12px; transition: color 0.1s;
  }
  .act-btn:hover { color: var(--text-primary, #e8eaed); }
  .act-btn.paused { color: #fbbf24; border-color: rgba(251,191,36,0.3); }

  /* ── Source filter ────────────────────────────────────────────────────────── */
  .source-bar {
    display: flex; flex-wrap: wrap; gap: 4px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    flex-shrink: 0;
  }
  .src-chip {
    background: none;
    border: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    color: var(--text-tertiary, #5f6368);
    padding: 2px 9px;
    border-radius: 12px;
    font-size: 11px;
    cursor: pointer;
    transition: all 0.1s;
  }
  .src-chip.active:not([style]) {
    border-color: var(--accent-primary, #5eead4);
    color: var(--accent-primary, #5eead4);
    background: rgba(94,234,212,0.1);
  }
  .src-chip:hover { color: var(--text-primary, #e8eaed); }

  /* ── Search ──────────────────────────────────────────────────────────────── */
  .search-row {
    padding: 6px 10px;
    border-bottom: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    flex-shrink: 0;
  }
  .dev-search {
    width: 100%; box-sizing: border-box;
    background: var(--bg-elevated, #1d2026);
    border: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    color: var(--text-primary, #e8eaed);
    padding: 5px 9px; border-radius: 5px; font-size: 12px;
    outline: none; transition: border-color 0.15s;
  }
  .dev-search:focus { border-color: var(--accent-primary, #5eead4); }

  /* ── Event list — flex:1 fills all remaining height ──────────────────────── */
  .event-list {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
  }

  .empty-state {
    padding: 32px 16px;
    text-align: center;
    color: var(--text-tertiary, #5f6368);
    font-size: 13px;
  }

  .ev-row {
    border-bottom: 1px solid var(--border-subtle, rgba(255,255,255,0.04));
    transition: background 0.1s;
  }
  .ev-row:hover { background: rgba(255,255,255,0.025) !important; }

  .ev-main {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    min-width: 0;
  }

  .ev-ts {
    font-size: 10px;
    color: var(--text-tertiary, #5f6368);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    flex-shrink: 0;
    min-width: 52px;
  }

  .src-dot {
    width: 7px; height: 7px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .ev-type {
    font-size: 11px;
    white-space: nowrap;
    flex-shrink: 0;
    font-weight: 500;
  }

  .ev-dev {
    font-size: 12px;
    color: var(--text-primary, #e8eaed);
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: 'JetBrains Mono', monospace;
  }

  .ev-actions {
    display: flex;
    gap: 3px;
    flex-shrink: 0;
    opacity: 0;
    transition: opacity 0.1s;
  }
  .ev-row:hover .ev-actions { opacity: 1; }

  .act-mini {
    background: none; border: none;
    color: var(--text-secondary, #9aa0a6);
    cursor: pointer; font-size: 13px; padding: 0 3px;
    line-height: 1;
  }
  .act-mini:hover { color: var(--accent-primary, #5eead4); }

  .ev-detail {
    padding: 4px 10px 7px 36px;
  }
  .ev-detail code {
    font-size: 11px;
    color: var(--text-secondary, #9aa0a6);
    white-space: pre-wrap;
    word-break: break-all;
    display: block;
  }
  .config-text {
    font-size: 11px;
    color: #4dd0c8;
    font-family: 'JetBrains Mono', monospace;
  }
</style>
