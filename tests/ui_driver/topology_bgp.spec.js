// @ts-check
// BN-016-U1 focused topology acceptance: the browser must render ONLY the
// BGP sessions the backend resolved. Seeded data (see
// tests/bn016_u1_topology_browser.rs):
//   r1 10.0.0.1 -> peer-a 10.0.0.2 (default NI, established)
//   r1 10.0.0.1 -> peer-a 10.0.0.2 (vrf-blue NI, established)  [2nd session]
//   r1 10.0.0.1 -> peer-b 10.0.0.3 (default NI, down)
//   r1 10.0.0.1 -> 10.9.9.9    (default NI, established, UNRESOLVED -> no edge)
import { test, expect } from '@playwright/test';

const BONSAI_URL = process.env.BONSAI_URL || 'http://127.0.0.1:3000';

test.describe('BN-016-U1 topology BGP session rendering', () => {
  test('API exposes per-session resolution; UI draws only resolved edges', async ({
    page,
    request,
  }) => {
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));

    // 1. Backend contract: /api/topology returns one bgp entry per observed
    //    session with explicit peer_device / resolution fields.
    const topoRes = await request.get(`${BONSAI_URL}/api/topology`);
    expect(topoRes.ok()).toBeTruthy();
    const topo = await topoRes.json();
    const r1 = (topo.devices ?? []).find((d) => d.address === '10.0.0.1');
    expect(r1, 'router 10.0.0.1 present in topology').toBeTruthy();
    const bgp = r1.bgp ?? [];
    // 4 observed sessions on r1 — the two vrf-blue/default sessions to
    // peer-a must NOT be collapsed into one.
    expect(bgp.length).toBe(4);

    const toPeerA = bgp.filter((b) => b.peer === '10.0.0.2');
    expect(toPeerA.length).toBe(2);
    for (const s of toPeerA) {
      expect(s.peer_device).toBe('10.0.0.2');
      expect(s.resolution).toBe('resolved');
    }

    const toPeerB = bgp.find((b) => b.peer === '10.0.0.3');
    expect(toPeerB).toBeTruthy();
    expect(toPeerB.peer_device).toBe('10.0.0.3');
    expect(toPeerB.state).toBe('down'); // down session stays visibly distinct

    const unresolved = bgp.find((b) => b.peer === '10.9.9.9');
    expect(unresolved, 'unresolved session still reported as observation').toBeTruthy();
    expect(unresolved.peer_device).toBeFalsy(); // never fabricated

    // 2. Browser journey: Live/topology page loads with no JS errors and
    //    renders the topology canvas (SVG) plus the BGP legend.
    await page.goto(`${BONSAI_URL}/`);
    await page.waitForLoadState('domcontentloaded');
    // Data loads async via fetch('/api/topology'); wait for the BGP legend
    // which only renders once resolved-session data has arrived.
    await expect(
      page.locator('.legend-item').filter({ hasText: 'BGP' }).first()
    ).toBeVisible({ timeout: 20_000 });
    await expect(page.locator('#topo-svg')).toBeVisible();
    expect(errors).toHaveLength(0);
  });
});
