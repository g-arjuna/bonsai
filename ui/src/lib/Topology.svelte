<script>
  import { onMount, createEventDispatcher } from 'svelte';
  import * as d3 from 'd3';

  const dispatch = createEventDispatcher();

  let loading = $state(true);
  let error = $state(null);
  let topology = $state({ devices: [], links: [] });
  let svgEl = $state(null);

  // --- Filter state ---
  let layerFilter = $state('combined'); // 'combined' | 'l3' | 'l2'
  let siteFilter  = $state('');         // '' = all, else site name
  let traceSrc    = $state(null);       // address of shift-click source
  let traceDst    = $state(null);       // address of shift-click destination
  let tracePath   = $state(null);       // { hops: [], links: [] }

  const HEALTH_COLOR = { healthy: '#3fb950', warn: '#d29922', critical: '#f85149' };

  // --- Role shapes (D3 custom symbol-like paths on a 28-radius circle bounding box) ---
  function roleShape(role, cx, cy) {
    const r = 28;
    const r2 = role === 'spine' ? r * 0.85 : r;
    switch ((role || '').toLowerCase()) {
      case 'spine': {
        // Square
        const s = r2 * 1.2;
        return `M${cx - s},${cy - s} h${s*2} v${s*2} h${-s*2} Z`;
      }
      case 'pe':
      case 'rr':
      case 'border': {
        // Hexagon
        const pts = Array.from({ length: 6 }, (_, i) => {
          const a = (Math.PI / 3) * i - Math.PI / 6;
          return [cx + r * Math.cos(a), cy + r * Math.sin(a)];
        });
        return 'M' + pts.map(p => p.join(',')).join('L') + 'Z';
      }
      default:
        // Circle (leaf / unknown)
        return null; // handled as <circle>
    }
  }

  // --- Link heatmap color ---
  const maxBytes = $derived(
    Math.max(1, ...topology.links.map(l => l.bytes_total ?? 0))
  );
  function linkColor(link) {
    if (!link.bytes_total) return '#30363d';
    const t = link.bytes_total / maxBytes;
    return d3.interpolateRdYlGn(1 - t * 0.85); // green=low, red=high
  }

  // --- Derived filtered data ---
  const sites = $derived([...new Set(topology.devices.map(d => d.site).filter(Boolean))].sort());

  const filteredDevices = $derived(
    siteFilter ? topology.devices.filter(d => d.site === siteFilter) : topology.devices
  );

  const filteredAddresses = $derived(new Set(filteredDevices.map(d => d.address)));

  const lldpLinks = $derived(
    topology.links.filter(l =>
      filteredAddresses.has(l.src_device) && filteredAddresses.has(l.dst_device)
    )
  );

  const bgpLinks = $derived(
    filteredDevices.flatMap(dev =>
      dev.bgp
        .filter(b => filteredAddresses.has(b.peer))
        .map(b => ({
          src_device: dev.address,
          src_iface: 'BGP',
          dst_device: b.peer,
          dst_iface: 'BGP',
          state: b.state,
          bytes_total: 0,
          isBgp: true,
        }))
    )
  );

  const visibleLinks = $derived(
    layerFilter === 'l3' ? bgpLinks :
    layerFilter === 'l2' ? lldpLinks :
    [...lldpLinks, ...bgpLinks]
  );

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
    } catch (e) {
      tracePath = { hops: [], links: [] };
    }
  }

  function handleNodeClick(event, address) {
    if (event.shiftKey) {
      if (!traceSrc) {
        traceSrc = address;
        traceDst = null;
        tracePath = null;
      } else if (traceSrc !== address) {
        traceDst = address;
        tracePathBetween(traceSrc, address);
      } else {
        // Re-click src clears trace
        traceSrc = null;
        traceDst = null;
        tracePath = null;
      }
    } else {
      dispatch('select', address);
    }
  }

  function clearTrace() {
    traceSrc = null;
    traceDst = null;
    tracePath = null;
  }

  function draw(devices, links) {
    if (!svgEl || !devices.length) return;

    const W = svgEl.clientWidth || 900;
    const H = 520;
    d3.select(svgEl).selectAll('*').remove();

    const svg = d3.select(svgEl).attr('viewBox', `0 0 ${W} ${H}`);
    const g = svg.append('g');
    svg.call(
      d3.zoom().scaleExtent([0.25, 5])
        .on('zoom', (event) => g.attr('transform', event.transform))
    );

    const pathHopSet = new Set(tracePath?.hops ?? []);
    const pathLinkSet = new Set(
      (tracePath?.links ?? []).map(([a, , b]) => [a, b].sort().join('|'))
    );

    const nodeMap = new Map(devices.map(d => [d.address, d]));
    const nodes = devices.map(d => ({ id: d.address, ...d }));

    // Deduplicate links (both LLDP and BGP)
    const seen = new Set();
    const simLinks = [];
    for (const l of links) {
      if (!nodeMap.has(l.src_device) || !nodeMap.has(l.dst_device)) continue;
      const key = [l.src_device, l.dst_device].sort().join('|');
      if (seen.has(key) && !l.isBgp) continue;
      seen.add(key);
      simLinks.push({ source: l.src_device, target: l.dst_device, ...l });
    }

    const sim = d3.forceSimulation(nodes)
      .force('link',      d3.forceLink(simLinks).id(d => d.id).distance(170))
      .force('charge',    d3.forceManyBody().strength(-700))
      .force('center',    d3.forceCenter(W / 2, H / 2))
      .force('collision', d3.forceCollide(50));

    // Links
    const link = g.append('g').selectAll('line').data(simLinks).join('line')
      .attr('stroke', l => {
        const key = [l.source.id ?? l.source, l.target.id ?? l.target].sort().join('|');
        if (tracePath && pathLinkSet.has(key)) return '#58a6ff';
        if (l.isBgp) return l.state === 'established' ? '#3fb950' : '#f85149';
        return linkColor(l);
      })
      .attr('stroke-width', l => {
        const key = [l.source.id ?? l.source, l.target.id ?? l.target].sort().join('|');
        return tracePath && pathLinkSet.has(key) ? 3 : 1.5;
      })
      .attr('stroke-dasharray', l => l.isBgp ? '5,3' : null)
      .attr('opacity', 0.85);

    link.append('title').text(l =>
      l.isBgp
        ? `BGP  ${l.src_device} ↔ ${l.dst_device}  [${l.state}]`
        : `${l.src_iface}  ↔  ${l.dst_iface}  (${(l.bytes_total / 1e9).toFixed(2)} GB)`
    );

    // Nodes
    const node = g.append('g').selectAll('g').data(nodes).join('g')
      .attr('cursor', 'pointer')
      .call(d3.drag()
        .on('start', (ev, d) => { if (!ev.active) sim.alphaTarget(0.3).restart(); d.fx = d.x; d.fy = d.y; })
        .on('drag',  (ev, d) => { d.fx = ev.x; d.fy = ev.y; })
        .on('end',   (ev, d) => { if (!ev.active) sim.alphaTarget(0); d.fx = null; d.fy = null; }));

    // Shape: circles for leaf/unknown, rect for spine, hexagon for pe/rr/border
    node.each(function(d) {
      const el = d3.select(this);
      const isOnPath = tracePath && pathHopSet.has(d.address);
      const strokeColor = isOnPath ? '#58a6ff' : (HEALTH_COLOR[d.health] || '#30363d');
      const strokeW = isOnPath ? 3.5 : 2.5;
      const role = (d.role || '').toLowerCase();

      if (role === 'spine') {
        const s = 24;
        el.append('rect')
          .attr('x', -s).attr('y', -s)
          .attr('width', s * 2).attr('height', s * 2)
          .attr('fill', '#161b22')
          .attr('stroke', strokeColor)
          .attr('stroke-width', strokeW)
          .attr('rx', 3);
      } else if (['pe', 'rr', 'border'].includes(role)) {
        const r = 28;
        const pts = Array.from({ length: 6 }, (_, i) => {
          const a = (Math.PI / 3) * i - Math.PI / 6;
          return [r * Math.cos(a), r * Math.sin(a)];
        });
        el.append('polygon')
          .attr('points', pts.map(p => p.join(',')).join(' '))
          .attr('fill', '#161b22')
          .attr('stroke', strokeColor)
          .attr('stroke-width', strokeW);
      } else {
        el.append('circle')
          .attr('r', 28)
          .attr('fill', '#161b22')
          .attr('stroke', strokeColor)
          .attr('stroke-width', strokeW);
      }

      // Trace source/dest indicator
      if (traceSrc === d.address) {
        el.append('circle').attr('r', 6).attr('cx', 20).attr('cy', -20)
          .attr('fill', '#58a6ff');
      }
      if (traceDst === d.address) {
        el.append('circle').attr('r', 6).attr('cx', 20).attr('cy', -20)
          .attr('fill', '#f0883e');
      }
    });

    // Labels
    node.append('text')
      .attr('text-anchor', 'middle').attr('dy', '-0.2em')
      .attr('font-size', 10).attr('fill', '#e6edf3').attr('pointer-events', 'none')
      .text(d => d.hostname || d.address.split(':')[0]);

    node.append('text')
      .attr('text-anchor', 'middle').attr('dy', '1.1em')
      .attr('font-size', 8).attr('fill', '#8b949e').attr('pointer-events', 'none')
      .text(d => (d.role ? `${d.role} · ` : '') + d.vendor.replace('nokia_', '').replace('cisco_', ''));

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
      <h2>Network Topology</h2>
      <span class="muted hint">scroll to zoom · drag to pan · shift+click to trace path</span>
    </div>

    <div class="topo-controls">
      <!-- Layer filter -->
      <div class="chip-group" role="group" aria-label="Layer filter">
        {#each [['combined','L2 + L3'],['l2','L2 only'],['l3','L3 only']] as [val, label]}
          <button class="chip {layerFilter === val ? 'active' : ''}"
                  onclick={() => layerFilter = val}>{label}</button>
        {/each}
      </div>

      <!-- Site scope -->
      {#if sites.length > 0}
        <select class="site-select" bind:value={siteFilter}
                aria-label="Filter by site">
          <option value="">All sites</option>
          {#each sites as s}
            <option value={s}>{s}</option>
          {/each}
        </select>
      {/if}

      <button class="ghost-btn" onclick={load}>Refresh</button>
    </div>
  </div>

  <!-- Path trace banner -->
  {#if traceSrc && !traceDst}
    <div class="trace-banner info">
      Tracing from <strong>{traceSrc}</strong> — shift+click a destination device.
      <button onclick={clearTrace}>Cancel</button>
    </div>
  {:else if tracePath}
    {#if tracePath.hops.length === 0}
      <div class="trace-banner warn">
        No path found between {traceSrc} and {traceDst}.
        <button onclick={clearTrace}>Clear</button>
      </div>
    {:else}
      <div class="trace-banner ok">
        Path ({tracePath.hops.length} hops): {tracePath.hops.join(' → ')}
        <button onclick={clearTrace}>Clear</button>
      </div>
    {/if}
  {/if}

  {#if loading}
    <p class="empty">Loading topology...</p>
  {:else if error}
    <p class="empty" style="color:var(--red)">Error: {error}</p>
  {:else if !topology.devices.length}
    <p class="empty">No devices found. Is bonsai running and connected to targets?</p>
  {:else}
    <svg id="topo-svg" bind:this={svgEl}></svg>

    <!-- Legend -->
    <div class="legend">
      <span class="legend-item"><span class="swatch circle" style="border-color:#3fb950"></span>Healthy</span>
      <span class="legend-item"><span class="swatch circle" style="border-color:#d29922"></span>Warn</span>
      <span class="legend-item"><span class="swatch circle" style="border-color:#f85149"></span>Critical</span>
      <span class="legend-item"><span class="shape-icon circle-icon"></span>Leaf</span>
      <span class="legend-item"><span class="shape-icon square-icon"></span>Spine</span>
      <span class="legend-item"><span class="shape-icon hex-icon"></span>PE/RR</span>
      <span class="legend-item"><span class="link-dash"></span>BGP session</span>
      <span class="legend-item">
        <span class="heatmap-bar"></span>Link utilisation
      </span>
    </div>

    <div class="card" style="margin-top:16px;">
      <table>
        <thead>
          <tr><th>Device</th><th>Role</th><th>Site</th><th>Vendor</th><th>Health</th><th>BGP Peers</th></tr>
        </thead>
        <tbody>
          {#each filteredDevices as d}
            <tr>
              <td><strong>{d.hostname}</strong><br><span class="muted" style="font-size:12px">{d.address}</span></td>
              <td>{d.role || '—'}</td>
              <td>{d.site || '—'}</td>
              <td>{d.vendor}</td>
              <td><span class="badge {d.health}">{d.health}</span></td>
              <td>
                {#each d.bgp as b}
                  <div style="font-size:12px">
                    {b.peer}{b.peer_as ? ` — AS${b.peer_as}` : ''}
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
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 12px;
    margin-bottom: 12px;
  }
  .topo-title { display: flex; align-items: baseline; gap: 12px; }
  .hint { font-size: 12px; }
  .topo-controls { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }

  .chip-group { display: flex; gap: 4px; }
  .chip {
    padding: 3px 10px;
    border: 1px solid var(--border);
    border-radius: 20px;
    background: transparent;
    color: var(--muted);
    font-size: 12px;
    cursor: pointer;
  }
  .chip.active { background: var(--blue); border-color: var(--blue); color: #fff; }

  .site-select {
    padding: 3px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg2);
    color: var(--text);
    font-size: 12px;
  }
  .ghost-btn {
    background: none;
    border: 1px solid var(--border);
    color: var(--muted);
    padding: 4px 12px;
    border-radius: 4px;
    cursor: pointer;
    font-size: 12px;
  }

  .trace-banner {
    display: flex; align-items: center; gap: 10px;
    padding: 8px 12px; border-radius: 4px; font-size: 13px; margin-bottom: 8px;
  }
  .trace-banner.info { background: rgba(88,166,255,0.12); border: 1px solid #58a6ff44; }
  .trace-banner.ok   { background: rgba(63,185,80,0.12);  border: 1px solid #3fb95044; }
  .trace-banner.warn { background: rgba(248,81,73,0.12);  border: 1px solid #f8514944; }
  .trace-banner button {
    margin-left: auto; background: none; border: none; color: var(--muted);
    cursor: pointer; font-size: 12px; text-decoration: underline;
  }

  #topo-svg { width: 100%; height: 520px; display: block; }

  .legend {
    display: flex; gap: 16px; flex-wrap: wrap;
    font-size: 11px; color: var(--muted); margin-top: 8px; padding: 0 4px;
  }
  .legend-item { display: flex; align-items: center; gap: 5px; }

  .swatch { width: 14px; height: 14px; border-radius: 50%; border: 2px solid; }
  .shape-icon { width: 14px; height: 14px; display: inline-block; }
  .circle-icon { border: 2px solid #8b949e; border-radius: 50%; }
  .square-icon { border: 2px solid #8b949e; border-radius: 2px; }
  .hex-icon {
    background: transparent;
    border: 2px solid #8b949e;
    clip-path: polygon(50% 0%,93% 25%,93% 75%,50% 100%,7% 75%,7% 25%);
  }
  .link-dash {
    width: 22px; height: 2px;
    background: repeating-linear-gradient(90deg, #3fb950 0, #3fb950 5px, transparent 5px, transparent 8px);
  }
  .heatmap-bar {
    width: 40px; height: 8px; border-radius: 2px;
    background: linear-gradient(to right, #3fb950, #d29922, #f85149);
  }
</style>
