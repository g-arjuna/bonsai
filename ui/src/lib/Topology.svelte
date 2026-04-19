<script>
  import { onMount, createEventDispatcher } from 'svelte';
  import * as d3 from 'd3';

  const dispatch = createEventDispatcher();

  let loading = $state(true);
  let error = $state(null);
  let topology = $state({ devices: [], links: [] });
  let svgEl = $state(null);

  const HEALTH_COLOR = { healthy: '#3fb950', warn: '#d29922', critical: '#f85149' };

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

  function draw(topo) {
    if (!svgEl || !topo.devices.length) return;

    const W = svgEl.clientWidth || 900;
    const H = 560;
    d3.select(svgEl).selectAll('*').remove();

    const svg = d3.select(svgEl).attr('viewBox', `0 0 ${W} ${H}`);

    // Zoom + pan container
    const g = svg.append('g');
    svg.call(
      d3.zoom()
        .scaleExtent([0.3, 4])
        .on('zoom', (event) => g.attr('transform', event.transform))
    );

    const nodeMap = new Map(topo.devices.map(d => [d.address, d]));
    const nodes = topo.devices.map(d => ({ id: d.address, ...d }));

    // Deduplicate links — LLDP gives us both directions
    const seen = new Set();
    const links = [];
    for (const l of topo.links) {
      if (!nodeMap.has(l.src_device) || !nodeMap.has(l.dst_device)) continue;
      const key = [l.src_device, l.dst_device].sort().join('|');
      if (seen.has(key)) continue;
      seen.add(key);
      links.push({ source: l.src_device, target: l.dst_device, ...l });
    }

    const sim = d3.forceSimulation(nodes)
      .force('link',      d3.forceLink(links).id(d => d.id).distance(160))
      .force('charge',    d3.forceManyBody().strength(-600))
      .force('center',    d3.forceCenter(W / 2, H / 2))
      .force('collision', d3.forceCollide(50));

    // Links
    const link = g.append('g')
      .selectAll('line')
      .data(links)
      .join('line')
      .attr('stroke', '#30363d')
      .attr('stroke-width', 1.5);

    // Hover tooltip on links showing interface names
    link.append('title').text(l => `${l.src_iface}  ↔  ${l.dst_iface}`);

    // Nodes
    const node = g.append('g')
      .selectAll('g')
      .data(nodes)
      .join('g')
      .attr('cursor', 'pointer')
      .call(d3.drag()
        .on('start', (ev, d) => { if (!ev.active) sim.alphaTarget(0.3).restart(); d.fx = d.x; d.fy = d.y; })
        .on('drag',  (ev, d) => { d.fx = ev.x; d.fy = ev.y; })
        .on('end',   (ev, d) => { if (!ev.active) sim.alphaTarget(0); d.fx = null; d.fy = null; }));

    node.append('circle')
      .attr('r', 28)
      .attr('fill', '#161b22')
      .attr('stroke', d => HEALTH_COLOR[d.health] || '#30363d')
      .attr('stroke-width', 2.5);

    // Hostname label inside circle
    node.append('text')
      .attr('text-anchor', 'middle')
      .attr('dy', '-0.2em')
      .attr('font-size', 10)
      .attr('fill', '#e6edf3')
      .attr('pointer-events', 'none')
      .text(d => d.hostname || d.address.split(':')[0]);

    // Vendor label below hostname
    node.append('text')
      .attr('text-anchor', 'middle')
      .attr('dy', '1.1em')
      .attr('font-size', 8)
      .attr('fill', '#8b949e')
      .attr('pointer-events', 'none')
      .text(d => d.vendor.replace('nokia_', '').replace('cisco_', ''));

    node.append('title').text(d =>
      `${d.hostname} — ${d.address}\nHealth: ${d.health}\nBGP peers: ${d.bgp.length}`
    );

    node.on('click', (_, d) => dispatch('trace', d.address));

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

  $effect(() => { draw(topology); });
</script>

<div class="view">
  <div style="display:flex; justify-content:space-between; align-items:center; margin-bottom:12px;">
    <h2>Network Topology</h2>
    <span style="color:var(--muted); font-size:12px">scroll to zoom · drag to pan · drag nodes to rearrange</span>
    <button onclick={load} style="background:none; border:1px solid var(--border); color:var(--muted); padding:4px 12px; border-radius:4px; cursor:pointer;">
      Refresh
    </button>
  </div>

  {#if loading}
    <p class="empty">Loading topology...</p>
  {:else if error}
    <p class="empty" style="color:var(--red)">Error: {error}</p>
  {:else if !topology.devices.length}
    <p class="empty">No devices found. Is bonsai running and connected to targets?</p>
  {:else}
    <svg id="topo-svg" bind:this={svgEl}></svg>

    <div class="card" style="margin-top:16px;">
      <table>
        <thead>
          <tr><th>Device</th><th>Vendor</th><th>Health</th><th>BGP Peers</th></tr>
        </thead>
        <tbody>
          {#each topology.devices as d}
            <tr>
              <td><strong>{d.hostname}</strong><br><span style="color:var(--muted); font-size:12px">{d.address}</span></td>
              <td>{d.vendor}</td>
              <td><span class="badge {d.health}">{d.health}</span></td>
              <td>
                {#each d.bgp as b}
                  <div style="font-size:12px">
                    {b.peer}{b.peer_as ? ` — AS${b.peer_as}` : ''}
                    <span class="badge {b.state === 'established' ? 'healthy' : 'critical'}">{b.state}</span>
                  </div>
                {/each}
                {#if !d.bgp.length}<span style="color:var(--muted)">none</span>{/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>
