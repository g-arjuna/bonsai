<script>
  import { onMount, createEventDispatcher } from 'svelte';
  import * as d3 from 'd3';
  import { C, healthColor, roleStrokeColor } from './design/colors.js';

  const dispatch = createEventDispatcher();

  // activeSite is driven by the parent (SiteRail) — Topology no longer owns site state.
  let { activeSite = '', onIncidentMapChange, onTopoLoad } = $props();

  let loading = $state(true);
  let error   = $state(null);
  let topology = $state({ devices: [], links: [], host_endpoints: [] });
  let svgEl    = $state(null);
  let showHosts = $state(false);
  let showMgmt  = $state(false);

  let incidentDevices = $state(new Map()); // address -> 'critical' | 'warn'

  // D4-12 T5: Redundancy group indication
  let redundancyMap = $state(new Map()); // address -> { state: 'ok'|'degraded'|'lost', group_type, protects }

  let layerFilter  = $state('combined');
  let selectedDevice = $state(null);
  let traceSrc     = $state(null);
  let traceDst     = $state(null);
  let tracePath    = $state(null);

  // ── Role alias map — covers DC fabric, campus, SP/WAN, wireless, cloud ──────
  // All aliases normalised to lowercase with hyphens/underscores/spaces stripped.
  const ROLE_TIER = {
    // Tier 0 — highest in hierarchy (core/superspine/route-reflectors)
    'superspine':0, 'superleaf':0,
    'core':0, 'backbone':0,
    'rr':0, 'routereflector':0,
    'wancore':0, 'wanrouter':0, 'wan':0,
    'datacentercore':0, 'dccore':0,
    'borderleaf':0,

    // Tier 1 — aggregation / distribution / spine / PE
    'spine':1, 'aggregation':1, 'distribution':1,
    'pe':1, 'providerededge':1,
    'border':1, 'borderrouter':1,
    'p':1,
    'wlc':1, 'wirelesscontroller':1, 'wlancontroller':1,
    'firewall':1, 'fw':1,
    'loadbalancer':1, 'lb':1,

    // Tier 2 — access / leaf / CE / edge
    'leaf':2, 'access':2, 'accessswitch':2,
    'ce':2, 'customeredge':2,
    'edge':2, 'edgerouter':2,
    'tor':2, 'toprack':2,
    'ap':2, 'accesspoint':2, 'wap':2,
    'cpe':2,
  };

  // ── Derived ────────────────────────────────────────────────────────────────
  const filteredDevices = $derived(
    activeSite ? topology.devices.filter(d => d.site === activeSite) : topology.devices
  );
  const filteredAddresses = $derived(new Set(filteredDevices.map(d => d.address)));

  const filteredHosts = $derived(
    showHosts
      ? (topology.host_endpoints ?? []).filter(h => !h.connected_to_device || filteredAddresses.has(h.connected_to_device))
      : []
  );

  const lldpLinks = $derived(
    topology.links.filter(l => !l.is_mgmt && filteredAddresses.has(l.src_device) && filteredAddresses.has(l.dst_device))
  );
  const mgmtLinks = $derived(
    showMgmt
      ? topology.links.filter(l => l.is_mgmt && filteredAddresses.has(l.src_device) && filteredAddresses.has(l.dst_device))
      : []
  );

  const bgpLinks = $derived(
    filteredDevices.flatMap(dev =>
      (dev.bgp ?? [])
        .map(b => ({ bgp: b, peerDevice: b.peer_device ?? b.peer_device_address ?? b.peer }))
        .filter(({ peerDevice }) => filteredAddresses.has(peerDevice))
        .map(({ bgp: b, peerDevice }) => ({
          src_device: dev.address, src_iface: 'BGP',
          dst_device: peerDevice, dst_iface: 'BGP',
          state: b.state, bytes_total: 0, isBgp: true,
        }))
    )
  );

  // True only when at least one device in the filtered set has BGP data
  const hasBgpData = $derived(filteredDevices.some(d => (d.bgp ?? []).length > 0));

  const unresolvedBgpSessions = $derived(
    filteredDevices.reduce((n, dev) =>
      n + (dev.bgp ?? []).filter(b => !filteredAddresses.has(b.peer_device ?? b.peer_device_address ?? b.peer)).length
    , 0)
  );

  const layerNotice = $derived(
    layerFilter === 'l3' && !bgpLinks.length && unresolvedBgpSessions
      ? 'BGP sessions exist but peers are loopback addresses — L3 edges cannot be rendered yet.'
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
    return d3.interpolateRgb('rgba(255,255,255,0.10)', C.accentPrimary)(t * 0.85 + 0.15);
  }

  // ── Data loading ──────────────────────────────────────────────────────────
  let lastRefresh = $state(null);

  async function load() {
    try {
      const [topoRes, incRes] = await Promise.all([
        fetch('/api/topology'),
        fetch('/api/incidents'),
      ]);
      if (!topoRes.ok) throw new Error(await topoRes.text());
      topology = await topoRes.json();
      if (incRes.ok) {
        const incData = await incRes.json();
        const map = new Map();
        for (const inc of incData.incidents ?? []) {
          const sev = inc.severity?.toLowerCase() ?? 'warn';
          for (const addr of inc.affected_devices ?? []) {
            const cur = map.get(addr);
            if (!cur || sev === 'critical') map.set(addr, sev);
          }
        }
        incidentDevices = map;
        onIncidentMapChange?.(map);
      }
      // D4-12 T5: Fetch redundancy groups
      try {
        const rgRes = await fetch('/api/redundancy/groups');
        if (rgRes.ok) {
          const rgData = await rgRes.json();
          const rMap = new Map();
          for (const rg of rgData.groups ?? []) {
            const state = rg.member_count <= 1 ? 'lost' : rg.member_count < rg.original_member_count ? 'degraded' : 'ok';
            for (const memberId of rg.member_node_ids ?? []) {
              const existing = rMap.get(memberId);
              // Keep worst state
              if (!existing || (state === 'lost') || (state === 'degraded' && existing.state === 'ok')) {
                rMap.set(memberId, { state, group_type: rg.group_type || rg.type, protects: rg.protects_node_id || '' });
              }
            }
          }
          redundancyMap = rMap;
        }
      } catch {}

      onTopoLoad?.(topology);
      lastRefresh = Date.now();
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
      tracePath = r.ok ? await r.json() : { hops: [], links: [] };
    } catch { tracePath = { hops: [], links: [] }; }
  }

  function handleNodeClick(event, address) {
    if (event.shiftKey) {
      if (!traceSrc)                  { traceSrc = address; traceDst = null; tracePath = null; }
      else if (traceSrc !== address)  { traceDst = address; tracePathBetween(traceSrc, address); }
      else                            { traceSrc = null; traceDst = null; tracePath = null; }
    } else {
      selectedDevice = selectedDevice === address ? null : address;
      dispatch('select', address);
    }
  }

  function clearTrace() { traceSrc = null; traceDst = null; tracePath = null; }

  // ── D3 drawing ────────────────────────────────────────────────────────────
  function draw(devices, links, hostEndpoints = []) {
    if (!svgEl || !devices.length) return;

    const W = svgEl.clientWidth || 900;
    const H = svgEl.clientHeight || 480;
    d3.select(svgEl).selectAll('*').remove();

    const svg = d3.select(svgEl).attr('viewBox', `0 0 ${W} ${H}`);

    const defs = svg.append('defs');
    defs.append('pattern')
      .attr('id', 'topo-grid').attr('width', 32).attr('height', 32)
      .attr('patternUnits', 'userSpaceOnUse')
      .append('circle').attr('cx', 1).attr('cy', 1).attr('r', 0.8)
      .attr('fill', 'rgba(255,255,255,0.035)');

    svg.append('rect').attr('width', W).attr('height', H).attr('fill', 'url(#topo-grid)');

    const g = svg.append('g');
    svg.call(d3.zoom().scaleExtent([0.2, 6]).on('zoom', ev => g.attr('transform', ev.transform)));

    const pathHopSet  = new Set(tracePath?.hops ?? []);
    const pathLinkSet = new Set((tracePath?.links ?? []).map(([a,, b]) => [a, b].sort().join('|')));

    const nodeMap  = new Map(devices.map(d => [d.address, d]));
    const nodes    = devices.map(d => ({ id: d.address, _isHost: false, ...d }));

    const hostNodes = hostEndpoints.map(h => ({
      id: `host:${h.id}`, _isHost: true,
      address: h.ip || h.id, hostname: h.hostname || h.ip || h.id,
      ip: h.ip, kind: h.kind, connected_to_device: h.connected_to_device,
      x: 0, y: 0,
    }));
    const allNodes   = [...nodes, ...hostNodes];
    const allNodeMap = new Map(allNodes.map(n => [n.id, n]));

    const hostSimLinks = hostNodes
      .filter(h => h.connected_to_device && nodeMap.has(h.connected_to_device))
      .map(h => ({ source: h.id, target: h.connected_to_device, _isHostLink: true }));

    const seen = new Set();
    const simLinks = [];
    for (const l of links) {
      if (!nodeMap.has(l.src_device) || !nodeMap.has(l.dst_device)) continue;
      const key = [l.src_device, l.dst_device].sort().join('|');
      if (seen.has(key) && !l.isBgp) continue;
      seen.add(key);
      simLinks.push({ source: l.src_device, target: l.dst_device, ...l });
    }
    const allSimLinks = [...simLinks, ...hostSimLinks];

    // ── Environment-agnostic tier assignment ─────────────────────────────────
    // 1. Role lookup with alias table (primary path)
    // 2. Fabric degree percentile (universal fallback — works for any topology)
    const fabricDegree = new Map(allNodes.map(n => [n.id, 0]));
    for (const l of links) {
      if (l.isBgp) continue;
      fabricDegree.set(l.src_device, (fabricDegree.get(l.src_device) ?? 0) + 1);
      fabricDegree.set(l.dst_device, (fabricDegree.get(l.dst_device) ?? 0) + 1);
    }
    const degs = [...nodes.map(n => fabricDegree.get(n.id) ?? 0)].sort((a, b) => b - a);
    const p25  = degs[Math.max(0, Math.floor(degs.length * 0.25))] ?? 1;
    const p75  = degs[Math.min(degs.length - 1, Math.floor(degs.length * 0.75))] ?? 0;

    function nodeTier(d) {
      const role = (d.role || '').toLowerCase().replace(/[-_\s]/g, '');
      if (role && role in ROLE_TIER) return ROLE_TIER[role];
      // Degree-based auto-tier: top 25% → tier 0, bottom 25% → tier 2, rest → tier 1
      const deg = fabricDegree.get(d.id) ?? 0;
      return deg >= p25 ? 0 : deg <= p75 ? 2 : 1;
    }

    nodes.forEach(n => { n._tier = nodeTier(n); });
    const usedTiers  = [...new Set(nodes.map(n => n._tier))].sort((a, b) => a - b);
    const tierYMap   = new Map(
      usedTiers.length === 1
        ? [[usedTiers[0], H * 0.5]]
        : usedTiers.map((t, i) => [t, H * (0.13 + 0.68 * i / (usedTiers.length - 1))])
    );
    const tierCounts = new Map(usedTiers.map(t => [t, nodes.filter(n => n._tier === t).length]));
    const tierOffset = new Map(usedTiers.map(t => [t, 0]));
    nodes.forEach(n => {
      const t = n._tier;
      tierOffset.set(t, tierOffset.get(t) + 1);
      n.x = (W / (tierCounts.get(t) + 1)) * tierOffset.get(t);
      n.y = tierYMap.get(t);
    });

    // Tier rail label: derive from actual roles of nodes in that tier
    function tierLabel(t) {
      const labels = new Set();
      for (const n of nodes.filter(nd => nd._tier === t)) {
        if (n.role) labels.add(n.role.replace(/[-_]/g, ' ').replace(/\b\w/g, c => c.toUpperCase()));
      }
      const idx = usedTiers.indexOf(t);
      const fallbacks = ['Core / Spine', 'Distribution / Aggregation', 'Access / Leaf'];
      return labels.size ? [...labels].slice(0, 3).join(' / ') : (fallbacks[idx] ?? `Tier ${t}`);
    }

    const maxTier = usedTiers.length ? Math.max(...usedTiers) : 2;
    hostNodes.forEach(h => { h._tier = maxTier + 1; });

    const sim = d3.forceSimulation(allNodes)
      .force('link',      d3.forceLink(allSimLinks).id(d => d.id).distance(90))
      .force('charge',    d3.forceManyBody().strength(-450))
      .force('y',         d3.forceY(d => d._isHost ? H * 0.9 : (tierYMap.get(d._tier) ?? H * 0.5)).strength(0.9))
      .force('x',         d3.forceX(W / 2).strength(0.03))
      .force('collision', d3.forceCollide(d => d._isHost ? 28 : 58));

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

    // Host dashed connectors (drawn below everything)
    const hostLinkSel = g.append('g').selectAll('line').data(hostSimLinks).join('line')
      .attr('stroke', 'rgba(255,255,255,0.15)')
      .attr('stroke-width', 1)
      .attr('stroke-dasharray', '3,3')
      .attr('opacity', 0.55);

    // Device links
    const link = g.append('g').selectAll('line').data(simLinks).join('line')
      .attr('stroke', l => {
        const key = [l.source.id ?? l.source, l.target.id ?? l.target].sort().join('|');
        if (tracePath && pathLinkSet.has(key)) return C.accentPrimary;
        if (l.is_mgmt) return C.textTertiary;
        if (l.isBgp)   return l.state === 'established' ? C.stateHealthy : C.stateFailed;
        return linkColor(l);
      })
      .attr('stroke-width', l => {
        const key = [l.source.id ?? l.source, l.target.id ?? l.target].sort().join('|');
        return tracePath && pathLinkSet.has(key) ? 2.5 : 1.5;
      })
      .attr('stroke-dasharray', l => l.is_mgmt ? '4,4' : l.isBgp ? '5,3' : null)
      .attr('opacity', l => l.is_mgmt ? 0.35 : 0.72);

    link.append('title').text(l =>
      l.is_mgmt ? `MGMT  ${l.src_iface} ↔ ${l.dst_iface}  (out-of-band)`
      : l.isBgp ? `BGP  ${l.src_device} ↔ ${l.dst_device}  [${l.state}]`
      : `${l.src_iface} ↔ ${l.dst_iface}  (${(l.bytes_total / 1e9).toFixed(2)} GB)`
    );

    // Host endpoint diamonds
    if (hostNodes.length) {
      const hostGroup = g.append('g').selectAll('g').data(hostNodes).join('g').attr('cursor', 'default');
      hostGroup.append('polygon')
        .attr('points', '0,-13 13,0 0,13 -13,0')
        .attr('fill', 'rgba(16,185,129,0.1)')
        .attr('stroke', '#10b981').attr('stroke-width', 1.5);
      hostGroup.append('text')
        .attr('text-anchor', 'middle').attr('dy', '0.35em')
        .attr('font-size', 7).attr('fill', '#10b981').attr('pointer-events', 'none')
        .text(d => d.hostname.length > 10 ? d.hostname.slice(0, 9) + '…' : d.hostname);
      hostGroup.append('title').text(d =>
        `${d.hostname}\nIP: ${d.ip}\nKind: ${d.kind || 'host'}\nConnected to: ${d.connected_to_device || '—'}`
      );
      sim.on('tick.hosts', () => hostGroup.attr('transform', d => `translate(${d.x ?? 0},${d.y ?? 0})`));
    }

    // Device nodes
    const node = g.append('g').selectAll('g').data(nodes).join('g')
      .attr('cursor', 'pointer')
      .call(d3.drag()
        .on('start', (ev, d) => { if (!ev.active) sim.alphaTarget(0.3).restart(); d.fx = d.x; d.fy = d.y; })
        .on('drag',  (ev, d) => { d.fx = ev.x; d.fy = ev.y; })
        .on('end',   (ev, d) => { if (!ev.active) sim.alphaTarget(0); d.fx = null; d.fy = null; })
      );

    node.each(function(d) {
      const el   = d3.select(this);
      const role = (d.role || '').toLowerCase().replace(/[-_\s]/g, '');

      const isSelected = selectedDevice === d.address;
      const isOnPath   = !!(tracePath && pathHopSet.has(d.address));
      const isTraceSrc = traceSrc === d.address;
      const isTraceDst = traceDst === d.address;

      const roleColor   = roleStrokeColor(d.role);
      const hColor      = healthColor(d.health);
      const incSev      = incidentDevices.get(d.address);
      const incColor    = incSev === 'critical' ? C.stateFailed : incSev === 'warn' ? C.stateDegraded : null;
      const strokeColor = isOnPath || isSelected ? C.accentPrimary : incColor ?? roleColor ?? hColor;
      const strokeW     = isSelected || isOnPath ? 3 : incColor ? 2.5 : 2;

      if (incColor && !isSelected && !isOnPath) {
        el.append('circle').attr('r', 35).attr('fill', 'none')
          .attr('stroke', incColor).attr('stroke-width', 1.5).attr('opacity', 0.35);
      }
      if (isSelected || isOnPath) {
        el.append('circle').attr('r', 35).attr('fill', 'none')
          .attr('stroke', strokeColor).attr('stroke-width', 1).attr('opacity', 0.25);
      }

      // Shape by tier: tier-0 → square (high-tier spine/core), tier-1 → hexagon, tier-2 → circle
      const tier = d._tier ?? 2;
      if (tier === 0) {
        const s = 26;
        el.append('rect').attr('x', -s).attr('y', -s).attr('width', s*2).attr('height', s*2)
          .attr('fill', C.bgSurface).attr('stroke', strokeColor).attr('stroke-width', strokeW).attr('rx', 3);
      } else if (tier === 1 || ['pe','rr','border','wlc','wirelesscontroller','wlancontroller','firewall','fw'].includes(role)) {
        const r = 24;
        const pts = Array.from({length:6}, (_,i) => { const a=(Math.PI/3)*i-Math.PI/6; return [r*Math.cos(a), r*Math.sin(a)]; });
        el.append('polygon').attr('points', pts.map(p=>p.join(',')).join(' '))
          .attr('fill', C.bgSurface).attr('stroke', strokeColor).attr('stroke-width', strokeW);
      } else {
        el.append('circle').attr('r', 26)
          .attr('fill', C.bgSurface).attr('stroke', strokeColor).attr('stroke-width', strokeW);
      }

      if (isTraceSrc) el.append('circle').attr('r', 5).attr('cx', 17).attr('cy', -17).attr('fill', C.accentPrimary);
      if (isTraceDst) el.append('circle').attr('r', 5).attr('cx', 17).attr('cy', -17).attr('fill', C.stateDegraded);

      // D4-12 T5: Redundancy group icon (chain link)
      const rgInfo = redundancyMap.get(d.address);
      if (rgInfo) {
        const rgColor = rgInfo.state === 'lost' ? C.stateFailed
                      : rgInfo.state === 'degraded' ? C.stateDegraded
                      : 'rgba(88,166,255,0.6)';
        // Small chain-link icon at bottom-right of node
        const iconG = el.append('g').attr('transform', 'translate(20, 18)');
        iconG.append('circle').attr('r', 8).attr('fill', C.bgSurface).attr('stroke', rgColor).attr('stroke-width', 1.5);
        // Two interlocking rings (simplified chain icon)
        iconG.append('circle').attr('cx', -2).attr('cy', 0).attr('r', 3.5)
          .attr('fill', 'none').attr('stroke', rgColor).attr('stroke-width', 1.2);
        iconG.append('circle').attr('cx', 2).attr('cy', 0).attr('r', 3.5)
          .attr('fill', 'none').attr('stroke', rgColor).attr('stroke-width', 1.2);
        iconG.append('title').text(
          rgInfo.state === 'lost' ? `Redundancy LOST (${rgInfo.group_type}) — single point of failure${rgInfo.protects ? '\nProtects: ' + rgInfo.protects : ''}`
          : rgInfo.state === 'degraded' ? `Redundancy DEGRADED (${rgInfo.group_type})${rgInfo.protects ? '\nProtects: ' + rgInfo.protects : ''}`
          : `Redundancy OK (${rgInfo.group_type})${rgInfo.protects ? '\nProtects: ' + rgInfo.protects : ''}`
        );
      }
    });

    node.append('text')
      .attr('text-anchor', 'middle').attr('dy', '-0.15em')
      .attr('font-size', 9).attr('font-family', "'JetBrains Mono', monospace")
      .attr('fill', C.textPrimary).attr('pointer-events', 'none')
      .text(d => d.hostname || d.address.split(':')[0]);

    node.append('text')
      .attr('text-anchor', 'middle').attr('dy', '1.1em')
      .attr('font-size', 8).attr('font-family', "'Inter', sans-serif")
      .attr('fill', C.textTertiary).attr('pointer-events', 'none')
      .text(d => d.site || d.vendor?.replace(/nokia_|cisco_|arista_/, '') || '');

    node.append('title').text(d => {
      const inc = incidentDevices.get(d.address);
      return `${d.hostname} — ${d.address}\nRole: ${d.role || 'unknown'}\nSite: ${d.site || '—'}\nHealth: ${d.health}${inc ? `\nIncident: ${inc} (open)` : ''}\nShift+click to trace path`;
    });
    node.on('click', (ev, d) => handleNodeClick(ev, d.address));

    sim.on('tick', () => {
      link
        .attr('x1', d => d.source.x).attr('y1', d => d.source.y)
        .attr('x2', d => d.target.x).attr('y2', d => d.target.y);
      hostLinkSel
        .attr('x1', l => (allNodeMap.get(typeof l.source==='object'?l.source.id:l.source)??{}).x??0)
        .attr('y1', l => (allNodeMap.get(typeof l.source==='object'?l.source.id:l.source)??{}).y??0)
        .attr('x2', l => (allNodeMap.get(typeof l.target==='object'?l.target.id:l.target)??{}).x??0)
        .attr('y2', l => (allNodeMap.get(typeof l.target==='object'?l.target.id:l.target)??{}).y??0);
      node.attr('transform', d => `translate(${d.x},${d.y})`);
    });
  }

  onMount(() => {
    load();
    const interval = setInterval(load, 15000);
    return () => clearInterval(interval);
  });

  $effect(() => { draw(filteredDevices, visibleLinks, filteredHosts); });
</script>

<div class="topo-wrap">
  <div class="topo-toolbar">
    <div class="chip-group" role="group" aria-label="Layer filter">
      {#each [['combined','All layers'],['l2','Fabric (L2)'],['l3','Routing (L3)']] as [val, label]}
        <button class="chip {layerFilter === val ? 'active' : ''}" onclick={() => layerFilter = val}>{label}</button>
      {/each}
    </div>

    <div class="toolbar-right">
      <button class="chip {showMgmt ? 'active' : ''}" onclick={() => showMgmt = !showMgmt}
              title="Show out-of-band management links">Mgmt</button>
      {#if (topology.host_endpoints ?? []).length}
        <button class="chip {showHosts ? 'active' : ''}" onclick={() => showHosts = !showHosts}>
          Hosts ({topology.host_endpoints.length})
        </button>
      {/if}
      <button class="ghost-btn" onclick={load} title="Refresh topology">↺</button>
    </div>
  </div>

  {#if traceSrc && !traceDst}
    <div class="trace-banner info">Tracing from <strong>{traceSrc}</strong> — shift+click a destination.
      <button onclick={clearTrace}>Cancel</button></div>
  {:else if tracePath}
    {#if tracePath.hops.length === 0}
      <div class="trace-banner warn">No path found. <button onclick={clearTrace}>Clear</button></div>
    {:else}
      <div class="trace-banner ok">{tracePath.hops.length} hops: {tracePath.hops.join(' → ')}
        <button onclick={clearTrace}>Clear</button></div>
    {/if}
  {/if}

  {#if layerNotice}
    <div class="trace-banner warn">{layerNotice}</div>
  {/if}

  {#if loading}
    <div class="canvas-placeholder"><span class="spin"></span><span>Loading…</span></div>
  {:else if error}
    <div class="canvas-placeholder err">Error: {error}</div>
  {:else if !topology.devices.length}
    <div class="canvas-placeholder">No devices found. Is bonsai running and connected to targets?</div>
  {:else}
    <svg id="topo-svg" bind:this={svgEl}></svg>

    <div class="legend">
      <span class="legend-item"><span class="swatch" style="border-color:#34d399"></span>Healthy</span>
      <span class="legend-item"><span class="swatch" style="border-color:#fbbf24"></span>Warn</span>
      <span class="legend-item"><span class="swatch" style="border-color:#f87171"></span>Critical</span>
      <span class="legend-item"><span class="shape sq"></span>Core/Spine</span>
      <span class="legend-item"><span class="shape hex"></span>Distribution</span>
      <span class="legend-item"><span class="shape circ"></span>Access/Leaf</span>
      {#if hasBgpData}
        <span class="legend-item"><span class="link-dash"></span>BGP</span>
      {/if}
      <span class="legend-item"><span class="heatmap-bar"></span>Link util</span>
      {#if showHosts}
        <span class="legend-item"><span class="shape diamond"></span>Host</span>
      {/if}
    </div>

    <div class="device-table-wrap">
      {#if showHosts && filteredHosts.length}
        <div class="card" style="margin-bottom:10px">
          <table>
            <thead><tr><th>Host</th><th>IP</th><th>Kind</th><th>Connected to</th><th>Interface</th></tr></thead>
            <tbody>
              {#each filteredHosts as h}
                <tr>
                  <td><strong>{h.hostname || '—'}</strong></td>
                  <td><code>{h.ip || '—'}</code></td>
                  <td>{h.kind || '—'}</td>
                  <td><code class="dim">{h.connected_to_device || '—'}</code></td>
                  <td><code>{h.connected_to_iface || '—'}</code></td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}

      <div class="card">
        <table>
          <thead>
            <tr>
              <th>Device</th>
              <th>Role</th>
              <th>Site</th>
              <th>Vendor</th>
              <th>Health</th>
              {#if hasBgpData}<th>BGP Peers</th>{/if}
            </tr>
          </thead>
          <tbody>
            {#each filteredDevices as d}
              <tr class:selected-row={selectedDevice === d.address}
                  onclick={() => handleNodeClick({}, d.address)}>
                <td>
                  <strong>{d.hostname}</strong><br>
                  <code class="dim">{d.address}</code>
                </td>
                <td>{d.role || '—'}</td>
                <td>{d.site || '—'}</td>
                <td><code>{d.vendor}</code></td>
                <td><span class="badge {d.health}">{d.health}</span></td>
                {#if hasBgpData}
                  <td>
                    {#each (d.bgp ?? []) as b}
                      <div class="bgp-row">
                        <code>{b.peer}</code>{b.peer_as ? ` AS${b.peer_as}` : ''}
                        <span class="badge {b.state === 'established' ? 'healthy' : 'critical'}">{b.state}</span>
                      </div>
                    {/each}
                    {#if !(d.bgp ?? []).length}<span class="dim">—</span>{/if}
                  </td>
                {/if}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </div>
  {/if}
</div>

<style>
  .topo-wrap {
    display: flex;
    flex-direction: column;
    height: 100%;
    overflow: hidden;
  }

  .topo-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 12px;
    border-bottom: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    flex-shrink: 0;
    gap: 8px;
    flex-wrap: wrap;
  }
  .toolbar-right { display: flex; align-items: center; gap: 6px; }

  .chip-group { display: flex; gap: 3px; }
  .chip {
    padding: 3px 10px;
    border: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    border-radius: 20px;
    background: transparent;
    color: var(--text-secondary, #9aa0a6);
    font-size: 12px;
    cursor: pointer;
    transition: background 0.1s, color 0.1s, border-color 0.1s;
  }
  .chip.active {
    background: rgba(94,234,212,0.1);
    border-color: var(--accent-primary, #5eead4);
    color: var(--accent-primary, #5eead4);
  }
  .chip:hover:not(.active) { color: var(--text-primary, #e8eaed); }

  .ghost-btn {
    background: none;
    border: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    color: var(--text-secondary, #9aa0a6);
    padding: 3px 10px;
    border-radius: 5px;
    cursor: pointer;
    font-size: 14px;
    transition: color 0.1s;
  }
  .ghost-btn:hover { color: var(--text-primary, #e8eaed); }

  #topo-svg {
    width: 100%;
    flex: 1;
    min-height: 0;
    display: block;
    background: var(--bg-surface, #15171b);
  }

  .device-table-wrap {
    overflow-y: auto;
    flex-shrink: 0;
    max-height: 32vh;
    border-top: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
  }

  .trace-banner {
    display: flex; align-items: center; gap: 10px;
    padding: 6px 12px; font-size: 12px; flex-shrink: 0;
  }
  .trace-banner.info { background: rgba(96,165,250,0.07);  border-bottom: 1px solid rgba(96,165,250,0.2); }
  .trace-banner.ok   { background: rgba(52,211,153,0.07);  border-bottom: 1px solid rgba(52,211,153,0.2); }
  .trace-banner.warn { background: rgba(248,113,113,0.07); border-bottom: 1px solid rgba(248,113,113,0.2); }
  .trace-banner button {
    margin-left: auto; background: none; border: none;
    color: var(--text-secondary, #9aa0a6); cursor: pointer; font-size: 12px; text-decoration: underline;
  }

  .canvas-placeholder {
    flex: 1; display: flex; align-items: center; justify-content: center;
    gap: 10px; color: var(--text-secondary, #9aa0a6); font-size: 13px;
  }
  .canvas-placeholder.err { color: #f87171; }

  .spin {
    width: 16px; height: 16px; border-radius: 50%;
    border: 2px solid rgba(255,255,255,0.1);
    border-top-color: var(--accent-primary, #5eead4);
    animation: spin 0.8s linear infinite;
    display: inline-block;
  }
  @keyframes spin { to { transform: rotate(360deg); } }

  .legend {
    display: flex; gap: 12px; flex-wrap: wrap;
    font-size: 11px; color: var(--text-secondary, #9aa0a6);
    padding: 5px 12px;
    border-top: 1px solid var(--border-subtle, rgba(255,255,255,0.06));
    flex-shrink: 0;
  }
  .legend-item { display: flex; align-items: center; gap: 4px; }
  .swatch { width: 10px; height: 10px; border-radius: 50%; border: 2px solid; }
  .shape { width: 11px; height: 11px; display: inline-block; flex-shrink: 0; }
  .circ    { border: 2px solid var(--text-secondary, #9aa0a6); border-radius: 50%; }
  .sq      { border: 2px solid var(--text-secondary, #9aa0a6); border-radius: 2px; }
  .diamond { border: 2px solid #10b981; transform: rotate(45deg); border-radius: 1px; }
  .hex {
    border: 2px solid var(--text-secondary, #9aa0a6);
    clip-path: polygon(50% 0%,93% 25%,93% 75%,50% 100%,7% 75%,7% 25%);
  }
  .link-dash {
    width: 18px; height: 2px;
    background: repeating-linear-gradient(90deg, #34d399 0, #34d399 4px, transparent 4px, transparent 7px);
  }
  .heatmap-bar {
    width: 30px; height: 6px; border-radius: 2px;
    background: linear-gradient(to right, #34d399, #fbbf24, #f87171);
  }

  .bgp-row { font-size: 11px; margin-bottom: 2px; }
  .dim { color: var(--text-tertiary, #5f6368); font-size: 11px; }
  code { font-size: 11px; }
  .selected-row td { background: rgba(94,234,212,0.05) !important; }
</style>
