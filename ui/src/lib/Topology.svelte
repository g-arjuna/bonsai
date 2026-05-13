<script>
  import { onMount, createEventDispatcher } from 'svelte';
  import * as d3 from 'd3';
  import { C, healthColor, roleStrokeColor } from './design/colors.js';

  const dispatch = createEventDispatcher();

  let loading = $state(true);
  let error = $state(null);
  let topology = $state({ devices: [], links: [] });
  let svgEl = $state(null);

  let layerFilter    = $state('combined');
  let siteFilter     = $state('');
  let showMgmt       = $state(false);
  let selectedDevice = $state(null);
  let traceSrc       = $state(null);
  let traceDst       = $state(null);
  let tracePath      = $state(null);

  // --- Derived filtered data ---
  const sites = $derived([...new Set(topology.devices.map(d => d.site).filter(Boolean))].sort());
  const filteredDevices = $derived(siteFilter ? topology.devices.filter(d => d.site === siteFilter) : topology.devices);
  const filteredAddresses = $derived(new Set(filteredDevices.map(d => d.address)));

  const lldpLinks = $derived(
    topology.links.filter(l => !l.is_mgmt && filteredAddresses.has(l.src_device) && filteredAddresses.has(l.dst_device))
  );
  const mgmtLinks = $derived(
    showMgmt ? topology.links.filter(l => l.is_mgmt && filteredAddresses.has(l.src_device) && filteredAddresses.has(l.dst_device)) : []
  );
  const bgpLinks = $derived(
    filteredDevices.flatMap(dev =>
      dev.bgp
        .map(b => ({ bgp: b, peerDevice: b.peer_device ?? b.peer_device_address ?? b.peer }))
        .filter(({ peerDevice }) => filteredAddresses.has(peerDevice))
        .map(({ bgp: b, peerDevice }) => ({
          src_device: dev.address, src_iface: 'BGP',
          dst_device: peerDevice, dst_iface: 'BGP',
          state: b.state, bytes_total: 0, isBgp: true,
        }))
    )
  );

  const unresolvedBgpSessions = $derived(
    filteredDevices.reduce((count, dev) => (
      count + dev.bgp.filter(b => {
        const peerDevice = b.peer_device ?? b.peer_device_address ?? b.peer;
        return !filteredAddresses.has(peerDevice);
      }).length
    ), 0)
  );

  const layerNotice = $derived(
    layerFilter === 'l3' && !bgpLinks.length && unresolvedBgpSessions
      ? 'BGP sessions present, but peers are reported as loopback addresses — L3 edges cannot be drawn yet.'
      : null
  );

  const visibleLinks = $derived(
    layerFilter === 'l3' ? [...bgpLinks, ...mgmtLinks] :
    layerFilter === 'l2' ? [...lldpLinks, ...mgmtLinks] :
    [...lldpLinks, ...bgpLinks, ...mgmtLinks]
  );

  const maxBytes = $derived(Math.max(1, ...topology.links.map(l => l.bytes_total ?? 0)));

  function linkColor(link) {
    if (!link.bytes_total) return C.borderDefault;
    const t = link.bytes_total / maxBytes;
    // Dim white → bright teal: high traffic = vivid, never red (red = failure elsewhere)
    return d3.interpolateRgb('rgba(255,255,255,0.12)', C.accentPrimary)(t * 0.85 + 0.15);
  }

  async function load() {
    try {
      const r = await fetch('/api/topology');
      if (!r.ok) throw new Error(await r.text());
      topology = await r.json();
      error = null;
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function tracePathBetween(src, dst) {
    try {
      const r = await fetch(`/api/path?src=${encodeURIComponent(src)}&dst=${encodeURIComponent(dst)}`);
      if (!r.ok) throw new Error(await r.text());
      tracePath = await r.json();
    } catch {
      tracePath = { hops: [], links: [] };
    }
  }

  function handleNodeClick(event, address) {
    if (event.shiftKey) {
      if (!traceSrc) {
        traceSrc = address; traceDst = null; tracePath = null;
      } else if (traceSrc !== address) {
        traceDst = address;
        tracePathBetween(traceSrc, address);
      } else {
        traceSrc = null; traceDst = null; tracePath = null;
      }
    } else {
      selectedDevice = selectedDevice === address ? null : address;
      dispatch('select', address);
    }
  }

  function clearTrace() {
    traceSrc = null; traceDst = null; tracePath = null;
  }

  function draw(devices, links) {
    if (!svgEl || !devices.length) return;

    const W = svgEl.clientWidth || 900;
    const H = 520;
    d3.select(svgEl).selectAll('*').remove();

    const svg = d3.select(svgEl).attr('viewBox', `0 0 ${W} ${H}`);

    // Subtle dot-grid background
    const defs = svg.append('defs');
    defs.append('pattern')
      .attr('id', 'topo-grid')
      .attr('width', 32).attr('height', 32)
      .attr('patternUnits', 'userSpaceOnUse')
      .append('circle')
        .attr('cx', 1).attr('cy', 1).attr('r', 0.8)
        .attr('fill', 'rgba(255,255,255,0.04)');

    svg.append('rect').attr('width', W).attr('height', H).attr('fill', 'url(#topo-grid)');

    const g = svg.append('g');
    svg.call(
      d3.zoom().scaleExtent([0.25, 5])
        .on('zoom', (event) => g.attr('transform', event.transform))
    );

    const pathHopSet  = new Set(tracePath?.hops ?? []);
    const pathLinkSet = new Set((tracePath?.links ?? []).map(([a,, b]) => [a, b].sort().join('|')));

    const nodeMap  = new Map(devices.map(d => [d.address, d]));
    const nodes    = devices.map(d => ({ id: d.address, ...d }));

    const seen = new Set();
    const simLinks = [];
    for (const l of links) {
      if (!nodeMap.has(l.src_device) || !nodeMap.has(l.dst_device)) continue;
      const key = [l.src_device, l.dst_device].sort().join('|');
      if (seen.has(key) && !l.isBgp) continue;
      seen.add(key);
      simLinks.push({ source: l.src_device, target: l.dst_device, ...l });
    }

    // Tier layout
    const ROLE_TIER = {
      'superspine': 0, 'core': 0, 'rr': 0, 'routereflector': 0,
      'spine': 1, 'p': 1, 'pe': 1, 'border': 1, 'distribution': 1, 'aggregation': 1,
      'leaf': 2, 'access': 2, 'ce': 2, 'edge': 2,
    };
    const TIER_FALLBACK = ['Aggregation', 'Distribution', 'Access'];

    const fabricDegree = new Map(nodes.map(n => [n.id, 0]));
    for (const l of links) {
      if (l.isBgp) continue;
      fabricDegree.set(l.src_device, (fabricDegree.get(l.src_device) ?? 0) + 1);
      fabricDegree.set(l.dst_device, (fabricDegree.get(l.dst_device) ?? 0) + 1);
    }
    const sortedDegs = [...fabricDegree.values()].sort((a, b) => b - a);
    const highDegCut = sortedDegs[Math.max(0, Math.floor(sortedDegs.length * 0.25))] ?? 1;
    const lowDegCut  = sortedDegs[Math.min(sortedDegs.length - 1, Math.floor(sortedDegs.length * 0.75))] ?? 0;

    function nodeTier(d) {
      const role = (d.role || '').toLowerCase().replace(/[-_ ]/g, '');
      const hn   = (d.hostname || '').toLowerCase();
      if (role === 'spine' && (hn.includes('super') || hn.startsWith('ss'))) return 0;
      if (role in ROLE_TIER) return ROLE_TIER[role];
      const deg = fabricDegree.get(d.id) ?? 0;
      return deg >= highDegCut ? 0 : deg <= lowDegCut ? 2 : 1;
    }

    nodes.forEach(n => { n._tier = nodeTier(n); });
    const usedTiers = [...new Set(nodes.map(n => n._tier))].sort((a, b) => a - b);
    const tierYMap = new Map(
      usedTiers.length === 1
        ? [[usedTiers[0], H * 0.5]]
        : usedTiers.map((t, i) => [t, H * (0.14 + (0.64 * i) / (usedTiers.length - 1))])
    );
    const tierCounts = new Map(usedTiers.map(t => [t, nodes.filter(n => n._tier === t).length]));
    const tierOffset = new Map(usedTiers.map(t => [t, 0]));
    nodes.forEach(n => {
      const t = n._tier;
      tierOffset.set(t, tierOffset.get(t) + 1);
      n.x = (W / (tierCounts.get(t) + 1)) * tierOffset.get(t);
      n.y = tierYMap.get(t);
    });

    function tierLabel(t) {
      const tierNodes = nodes.filter(n => n._tier === t);
      const labels = new Set();
      for (const n of tierNodes) {
        const role = (n.role || '').toLowerCase().replace(/[-_ ]/g, '');
        const hn = (n.hostname || '').toLowerCase();
        if (role === 'superspine' || (role === 'spine' && (hn.includes('super') || hn.startsWith('ss')))) labels.add('Super-Spine');
        else if (n.role) labels.add(n.role.charAt(0).toUpperCase() + n.role.slice(1));
      }
      if (!labels.size) {
        const idx = usedTiers.indexOf(t);
        return TIER_FALLBACK[Math.min(idx, TIER_FALLBACK.length - 1)];
      }
      return [...labels].slice(0, 3).join(' / ');
    }

    const sim = d3.forceSimulation(nodes)
      .force('link',      d3.forceLink(simLinks).id(d => d.id).distance(140))
      .force('charge',    d3.forceManyBody().strength(-500))
      .force('y',         d3.forceY(d => tierYMap.get(d._tier) ?? H * 0.5).strength(0.85))
      .force('x',         d3.forceX(W / 2).strength(0.04))
      .force('collision', d3.forceCollide(60));

    // Tier rail labels
    for (const t of usedTiers) {
      if (!tierCounts.get(t)) continue;
      g.append('text')
        .attr('x', 6).attr('y', tierYMap.get(t))
        .attr('dominant-baseline', 'middle')
        .attr('font-size', 9).attr('fill', C.textTertiary)
        .attr('pointer-events', 'none')
        .text(tierLabel(t));
    }

    // Links
    const link = g.append('g').selectAll('line').data(simLinks).join('line')
      .attr('stroke', l => {
        const key = [l.source.id ?? l.source, l.target.id ?? l.target].sort().join('|');
        if (tracePath && pathLinkSet.has(key)) return C.accentPrimary;
        if (l.is_mgmt)  return C.textTertiary;
        if (l.isBgp)    return l.state === 'established' ? C.stateHealthy : C.stateFailed;
        return linkColor(l);
      })
      .attr('stroke-width', l => {
        const key = [l.source.id ?? l.source, l.target.id ?? l.target].sort().join('|');
        return tracePath && pathLinkSet.has(key) ? 2.5 : 1.5;
      })
      .attr('stroke-dasharray', l => l.is_mgmt ? '4,4' : l.isBgp ? '5,3' : null)
      .attr('opacity', l => l.is_mgmt ? 0.4 : 0.75);

    link.append('title').text(l =>
      l.is_mgmt ? `MGMT  ${l.src_iface}  ↔  ${l.dst_iface}  (out-of-band)`
      : l.isBgp ? `BGP  ${l.src_device} ↔ ${l.dst_device}  [${l.state}]`
      : `${l.src_iface}  ↔  ${l.dst_iface}  (${(l.bytes_total / 1e9).toFixed(2)} GB)`
    );

    // Nodes
    const node = g.append('g').selectAll('g').data(nodes).join('g')
      .attr('cursor', 'pointer')
      .call(d3.drag()
        .on('start', (ev, d) => { if (!ev.active) sim.alphaTarget(0.3).restart(); d.fx = d.x; d.fy = d.y; })
        .on('drag',  (ev, d) => { d.fx = ev.x; d.fy = ev.y; })
        .on('end',   (ev, d) => { if (!ev.active) sim.alphaTarget(0); d.fx = null; d.fy = null; }));

    node.each(function(d) {
      const el = d3.select(this);
      const role = (d.role || '').toLowerCase();
      const isSelected  = selectedDevice === d.address;
      const isOnPath    = tracePath && pathHopSet.has(d.address);
      const isTraceSrc  = traceSrc === d.address;
      const isTraceDst  = traceDst === d.address;

      // Stroke: path > selected > role-based > health
      const roleColor   = roleStrokeColor(d.role, d.hostname);
      const hColor      = healthColor(d.health);
      const strokeColor =
        isOnPath   ? C.accentPrimary :
        isSelected ? C.accentPrimary :
        roleColor  ? roleColor :
        hColor;
      const strokeW     = isSelected || isOnPath ? 3 : 2;

      // Selection glow ring
      if (isSelected || isOnPath) {
        if (role === 'spine' || role === 'super-spine') {
          const s = role === 'super-spine' ? 38 : 34;
          el.append('rect')
            .attr('x', -s).attr('y', -s)
            .attr('width', s * 2).attr('height', s * 2)
            .attr('fill', 'none')
            .attr('stroke', strokeColor).attr('stroke-width', 1)
            .attr('rx', 6).attr('opacity', 0.3);
        } else {
          el.append('circle')
            .attr('r', 36)
            .attr('fill', 'none')
            .attr('stroke', strokeColor).attr('stroke-width', 1)
            .attr('opacity', 0.3);
        }
      }

      if (role === 'spine' || role === 'super-spine') {
        const s = role === 'super-spine' ? 32 : 28;
        el.append('rect')
          .attr('x', -s).attr('y', -s)
          .attr('width', s * 2).attr('height', s * 2)
          .attr('fill', C.bgSurface)
          .attr('stroke', strokeColor)
          .attr('stroke-width', strokeW)
          .attr('rx', 3);
      } else if (['pe', 'rr', 'border'].includes(role)) {
        const r = 26;
        const pts = Array.from({ length: 6 }, (_, i) => {
          const a = (Math.PI / 3) * i - Math.PI / 6;
          return [r * Math.cos(a), r * Math.sin(a)];
        });
        el.append('polygon')
          .attr('points', pts.map(p => p.join(',')).join(' '))
          .attr('fill', C.bgSurface)
          .attr('stroke', strokeColor)
          .attr('stroke-width', strokeW);
      } else {
        el.append('circle')
          .attr('r', 28)
          .attr('fill', C.bgSurface)
          .attr('stroke', strokeColor)
          .attr('stroke-width', strokeW);
      }

      // Trace endpoint dots
      if (isTraceSrc) el.append('circle').attr('r', 5).attr('cx', 18).attr('cy', -18).attr('fill', C.accentPrimary);
      if (isTraceDst) el.append('circle').attr('r', 5).attr('cx', 18).attr('cy', -18).attr('fill', C.stateDegraded);
    });

    // Labels
    node.append('text')
      .attr('text-anchor', 'middle').attr('dy', '-0.2em')
      .attr('font-size', 9)
      .attr('font-family', "'JetBrains Mono', monospace")
      .attr('fill', C.textPrimary)
      .attr('pointer-events', 'none')
      .text(d => d.hostname || d.address.split(':')[0]);

    node.append('text')
      .attr('text-anchor', 'middle').attr('dy', '1.1em')
      .attr('font-size', 8)
      .attr('font-family', "'Inter', sans-serif")
      .attr('fill', C.textTertiary)
      .attr('pointer-events', 'none')
      .text(d => d.site || d.vendor.replace('nokia_', '').replace('cisco_', ''));

    node.append('title').text(d =>
      `${d.hostname} — ${d.address}\nRole: ${d.role || 'unknown'}\nSite: ${d.site || '—'}\nHealth: ${d.health}\nShift+click to trace path`
    );

    node.on('click', (ev, d) => handleNodeClick(ev, d.address));

    sim.on('tick', () => {
      link
        .attr('x1', d => d.source.x).attr('y1', d => d.source.y)
        .attr('x2', d => d.target.x).attr('y2', d => d.target.y);
      node.attr('transform', d => `translate(${d.x},${d.y})`);
    });
  }

  onMount(() => {
    load();
    const interval = setInterval(load, 15000);
    return () => clearInterval(interval);
  });

  $effect(() => { draw(filteredDevices, visibleLinks); });
</script>

<div class="view">
  <div class="topo-header">
    <div class="topo-title">
      <p class="eyebrow">Graph</p>
      <h2>Network Topology</h2>
      <span class="hint">scroll to zoom · drag nodes · shift+click to trace path</span>
    </div>

    <div class="topo-controls">
      <div class="chip-group" role="group" aria-label="Layer filter">
        {#each [['combined','Fabric + BGP'],['l2','Fabric only'],['l3','BGP sessions']] as [val, label]}
          <button class="chip {layerFilter === val ? 'active' : ''}" onclick={() => layerFilter = val}>
            {label}
          </button>
        {/each}
      </div>

      {#if sites.length > 0}
        <select class="site-select" bind:value={siteFilter} aria-label="Filter by site">
          <option value="">All sites</option>
          {#each sites as s}<option value={s}>{s}</option>{/each}
        </select>
      {/if}

      <button class="chip {showMgmt ? 'active' : ''}" onclick={() => showMgmt = !showMgmt}
              title="Show out-of-band management-plane links">
        Mgmt links
      </button>

      <button class="ghost-btn" onclick={load}>Refresh</button>
    </div>
  </div>

  {#if traceSrc && !traceDst}
    <div class="trace-banner info">
      Tracing from <strong>{traceSrc}</strong> — shift+click a destination.
      <button onclick={clearTrace}>Cancel</button>
    </div>
  {:else if tracePath}
    {#if tracePath.hops.length === 0}
      <div class="trace-banner warn">No path found. <button onclick={clearTrace}>Clear</button></div>
    {:else}
      <div class="trace-banner ok">
        {tracePath.hops.length} hops: {tracePath.hops.join(' → ')}
        <button onclick={clearTrace}>Clear</button>
      </div>
    {/if}
  {/if}

  {#if layerNotice}
    <div class="trace-banner warn">{layerNotice}</div>
  {/if}

  {#if loading}
    <p class="empty">Loading topology…</p>
  {:else if error}
    <p class="empty" style="color:var(--state-failed)">Error: {error}</p>
  {:else if !topology.devices.length}
    <p class="empty">No devices found. Is bonsai running and connected to targets?</p>
  {:else}
    <svg id="topo-svg" bind:this={svgEl}></svg>

    <div class="legend">
      <span class="legend-item"><span class="swatch" style="border-color:var(--state-healthy)"></span>Healthy</span>
      <span class="legend-item"><span class="swatch" style="border-color:var(--state-degraded)"></span>Warn</span>
      <span class="legend-item"><span class="swatch" style="border-color:var(--state-failed)"></span>Critical</span>
      <span class="legend-item"><span class="shape circle-icon"></span>Leaf</span>
      <span class="legend-item"><span class="shape square-icon"></span>Spine</span>
      <span class="legend-item"><span class="shape hex-icon"></span>PE/RR</span>
      <span class="legend-item"><span class="link-dash"></span>BGP</span>
      <span class="legend-item"><span class="heatmap-bar"></span>Link utilisation</span>
    </div>

    <div class="card" style="margin-top:14px">
      <table>
        <thead>
          <tr><th>Device</th><th>Role</th><th>Site</th><th>Vendor</th><th>Health</th><th>BGP Peers</th></tr>
        </thead>
        <tbody>
          {#each filteredDevices as d}
            <tr class:selected-row={selectedDevice === d.address} onclick={() => handleNodeClick({}, d.address)}>
              <td>
                <strong>{d.hostname}</strong><br>
                <code style="font-size:11px;color:var(--text-tertiary)">{d.address}</code>
              </td>
              <td>{d.role || '—'}</td>
              <td>{d.site || '—'}</td>
              <td><code style="font-size:11px">{d.vendor}</code></td>
              <td><span class="badge {d.health}">{d.health}</span></td>
              <td>
                {#each d.bgp as b}
                  <div style="font-size:11px; margin-bottom:2px;">
                    <code>{b.peer}</code>{b.peer_as ? ` AS${b.peer_as}` : ''}
                    <span class="badge {b.state === 'established' ? 'healthy' : 'critical'}">{b.state}</span>
                  </div>
                {/each}
                {#if !d.bgp.length}<span class="muted">none</span>{/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<style>
  .topo-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 12px;
    margin-bottom: 12px;
  }
  .topo-title { display: flex; flex-direction: column; gap: 2px; }
  .topo-title h2 {
    font-size: var(--text-display-3);
    font-weight: 700;
    letter-spacing: var(--tracking-display);
    line-height: var(--leading-display);
    margin: 0;
  }
  .hint { font-size: 11px; color: var(--text-tertiary); }
  .topo-controls { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }

  .chip-group { display: flex; gap: 3px; }
  .chip {
    padding: 4px 10px;
    border: 1px solid var(--border-subtle);
    border-radius: 20px;
    background: transparent;
    color: var(--text-secondary);
    font-size: 12px;
    cursor: pointer;
    transition: background var(--duration-instant) var(--ease-out),
                color var(--duration-instant) var(--ease-out),
                border-color var(--duration-instant) var(--ease-out);
  }
  .chip.active {
    background: rgba(94,234,212,0.12);
    border-color: var(--accent-primary);
    color: var(--accent-primary);
  }
  .chip:hover:not(.active) { color: var(--text-primary); border-color: var(--border-default); }

  .site-select {
    padding: 4px 8px;
    border: 1px solid var(--border-subtle);
    border-radius: 5px;
    background: var(--bg-surface);
    color: var(--text-primary);
    font-size: 12px;
  }
  .ghost-btn {
    background: none;
    border: 1px solid var(--border-subtle);
    color: var(--text-secondary);
    padding: 4px 12px;
    border-radius: 5px;
    cursor: pointer;
    font-size: 12px;
    transition: color var(--duration-instant) var(--ease-out),
                border-color var(--duration-instant) var(--ease-out);
  }
  .ghost-btn:hover { color: var(--text-primary); border-color: var(--border-default); }

  .trace-banner {
    display: flex; align-items: center; gap: 10px;
    padding: 8px 12px; border-radius: 5px; font-size: 13px; margin-bottom: 8px;
  }
  .trace-banner.info { background: rgba(96,165,250,0.08);  border: 1px solid rgba(96,165,250,0.3); }
  .trace-banner.ok   { background: rgba(52,211,153,0.08);  border: 1px solid rgba(52,211,153,0.3); }
  .trace-banner.warn { background: rgba(248,113,113,0.08); border: 1px solid rgba(248,113,113,0.3); }
  .trace-banner button {
    margin-left: auto; background: none; border: none; color: var(--text-secondary);
    cursor: pointer; font-size: 12px; text-decoration: underline;
  }

  #topo-svg {
    width: 100%; height: 520px; display: block;
    background: var(--bg-surface);
    border-radius: 6px;
    border: 1px solid var(--border-subtle);
  }

  .legend {
    display: flex; gap: 14px; flex-wrap: wrap;
    font-size: 11px; color: var(--text-secondary); margin-top: 8px; padding: 0 2px;
  }
  .legend-item { display: flex; align-items: center; gap: 5px; }
  .swatch { width: 12px; height: 12px; border-radius: 50%; border: 2px solid; }
  .shape { width: 12px; height: 12px; display: inline-block; flex-shrink: 0; }
  .circle-icon { border: 2px solid var(--text-secondary); border-radius: 50%; }
  .square-icon { border: 2px solid var(--text-secondary); border-radius: 2px; }
  .hex-icon {
    border: 2px solid var(--text-secondary);
    clip-path: polygon(50% 0%,93% 25%,93% 75%,50% 100%,7% 75%,7% 25%);
  }
  .link-dash {
    width: 20px; height: 2px;
    background: repeating-linear-gradient(90deg, var(--state-healthy) 0, var(--state-healthy) 4px, transparent 4px, transparent 7px);
  }
  .heatmap-bar {
    width: 36px; height: 7px; border-radius: 2px;
    background: linear-gradient(to right, var(--state-healthy), var(--state-degraded), var(--state-failed));
  }

  .selected-row td { background: rgba(94,234,212,0.05) !important; }
</style>
