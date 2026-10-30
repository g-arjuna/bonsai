// @ts-check
import { test, expect } from '@playwright/test';
import fs from 'fs';
import path from 'path';

const BONSAI_URL = process.env.BONSAI_URL || 'http://127.0.0.1:31080';
const CTRL_URL = process.env.BONSAI_CTRL_URL || 'http://127.0.0.1:31081';
const SCOPED_KEY = process.env.BONSAI_SCOPED_KEY || 'bsk_scoped_key_for_testing_12345';
const EVIDENCE_DIR = process.env.BONSAI_EVIDENCE_DIR || 'docs/merger/evidence/2026-10-13-bn003';

test.describe('BN-003 Live Routing & RIB Browser Acceptance Gate', () => {
  test.describe.configure({ mode: 'serial', retries: 0 });

  let savedCookies = null;

  test.beforeAll(async () => {
    fs.mkdirSync(EVIDENCE_DIR, { recursive: true });
  });

  test.beforeEach(async ({ context }, testInfo) => {
    if (savedCookies && !testInfo.title.startsWith('1.')) {
      await context.addCookies(savedCookies);
    }
  });

  test('1. Unauthenticated access is denied with HTTP 401 in API and UI', async ({ page, request }) => {
    // API verification
    const apiRes = await request.get(`${BONSAI_URL}/api/routing/v1/health`);
    expect(apiRes.status()).toBe(401);

    // Browser UI verification without session token
    await page.goto(`${BONSAI_URL}/#/routing`);
    await page.waitForLoadState('domcontentloaded');

    // UI renders error alert for 401
    const alert = page.locator('.alert.alert-danger');
    await expect(alert).toBeVisible({ timeout: 10_000 });
    await expect(alert).toContainText(/401|Unavailable/i);

    await page.screenshot({ path: path.join(EVIDENCE_DIR, '00_unauthenticated_denied.png'), fullPage: true });
  });

  test('2. Cross-scope access is denied with HTTP 403', async ({ request }) => {
    // Cross-speaker access denied
    const crossSpkRes = await request.get(
      `${BONSAI_URL}/api/routing/v1/prefix-history?prefix=10.10.1.0/24&speaker=172.100.207.12`,
      {
        headers: { Authorization: `Bearer ${SCOPED_KEY}` },
      }
    );
    expect(crossSpkRes.status()).toBe(403);
    const spkJson = await crossSpkRes.json();
    expect(spkJson.error).toBe('routing_scope_violation');

    // Cross-VRF access denied
    const crossVrfRes = await request.get(
      `${BONSAI_URL}/api/routing/v1/prefix-history?prefix=10.10.1.0/24&vrf_rd=65000:100`,
      {
        headers: { Authorization: `Bearer ${SCOPED_KEY}` },
      }
    );
    expect(crossVrfRes.status()).toBe(403);
    const vrfJson = await crossVrfRes.json();
    expect(vrfJson.error).toBe('routing_scope_violation');
  });

  test('3. Authenticated login issues JWT Set-Cookie and loads Routing Observatory', async ({ page, context }) => {
    // Perform login via context-bound request API (page.request)
    const loginRes = await page.request.post(`${BONSAI_URL}/api/auth/login`, {
      data: {
        username: 'admin',
        password: process.env.BONSAI_ADMIN_PASS || 'BonsaiAdmin123!@#$',
      },
    });
    expect(loginRes.status()).toBe(200);
    const loginData = await loginRes.json();
    expect(loginData.success).toBe(true);
    expect(loginData.role).toBe('admin');

    // Inspect server-issued Set-Cookie processed by browser context
    const cookies = await context.cookies(BONSAI_URL);
    const tokenCookie = cookies.find(c => c.name === 'bonsai_token');
    expect(tokenCookie).toBeDefined();
    expect(tokenCookie.value).toBeTruthy();
    expect(tokenCookie.httpOnly).toBe(true);
    expect(tokenCookie.sameSite).toBe('Lax');
    expect(tokenCookie.path).toBe('/');

    savedCookies = cookies;

    // Navigate to /#/routing WITHOUT manual context.addCookies
    await page.goto(`${BONSAI_URL}/#/routing`);
    await page.waitForLoadState('domcontentloaded');

    // Workspace Header elements
    await expect(page.locator('.eyebrow')).toHaveText('Canonical BMP / BGP Control Plane');
    await expect(page.locator('h2')).toHaveText('Routing & RIB Observatory');

    // Summary KPI cards populated
    const speakersKpi = page.locator('.kpi-card', { hasText: 'BMP Speakers' }).locator('.kpi-value');
    await expect(speakersKpi).not.toHaveText('0', { timeout: 15_000 });

    const peersKpi = page.locator('.kpi-card', { hasText: 'BGP Peers Up' }).locator('.kpi-value');
    await expect(peersKpi).not.toHaveText('0', { timeout: 15_000 });

    const routesKpi = page.locator('.kpi-card', { hasText: 'Active RIB Routes' }).locator('.kpi-value');
    await expect(routesKpi).not.toHaveText('0', { timeout: 15_000 });

    // Header badge check: graph projection lag is 0
    const graphLagBadge = page.locator('.badge', { hasText: /Graph Lag/i });
    await expect(graphLagBadge).toBeVisible();
    await expect(graphLagBadge).toContainText('Graph Lag: 0', { timeout: 10_000 });

    // Exporters table has both frr1 (172.100.207.11) and frr2 (172.100.207.12)
    const exportersTable = page.locator('.panel', { hasText: 'BMP Exporters' }).locator('.data-table');
    await expect(exportersTable).toContainText('172.100.207.11');
    await expect(exportersTable).toContainText('172.100.207.12');

    await page.screenshot({ path: path.join(EVIDENCE_DIR, '01_routing_dashboard_initial.png'), fullPage: true });
  });

  test('4. RIB Explorer populates 3 views and verifies VRF and Speaker isolation', async ({ page }) => {
    await page.goto(`${BONSAI_URL}/#/routing`);
    await page.waitForLoadState('domcontentloaded');

    const viewSelect = page.locator('select[aria-label="RIB View"]');
    const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
    const ribTable = page.locator('.panel', { hasText: 'Canonical RIB Explorer' }).locator('.data-table');

    // 1. AdjRibInPrePolicy
    await viewSelect.selectOption('AdjRibInPrePolicy');
    await filterBtn.click();
    await page.waitForTimeout(500);
    await expect(ribTable).toContainText('AdjRibInPrePolicy');
    await expect(ribTable).toContainText('10.10.1.0/24');

    // 2. AdjRibInPostPolicy
    await viewSelect.selectOption('AdjRibInPostPolicy');
    await filterBtn.click();
    await page.waitForTimeout(500);
    await expect(ribTable).toContainText('AdjRibInPostPolicy');
    await expect(ribTable).toContainText('10.10.1.0/24');

    // 3. LocRib
    await viewSelect.selectOption('LocRib');
    await filterBtn.click();
    await page.waitForTimeout(500);
    await expect(ribTable).toContainText('LocRib');
    await expect(ribTable).toContainText('10.10.1.0/24');

    // 4. VRF RD isolation: filter by 65000:100 (deterministic test fixture)
    await viewSelect.selectOption('');
    const vrfInput = page.locator('input[aria-label="VRF RD Filter"]');
    await vrfInput.fill('65000:100');
    await filterBtn.click();
    await page.waitForTimeout(500);
    await expect(ribTable).toContainText('65000:100');
    await expect(ribTable).toContainText('10.99.1.0/24');
    // Global route must be strictly excluded from VRF RD view
    await expect(ribTable).not.toContainText('10.10.1.0/24');

    // 5. Global RD isolation: filter by 0:0
    await vrfInput.fill('0:0');
    await viewSelect.selectOption('');
    await filterBtn.click();
    await page.waitForTimeout(500);
    await expect(ribTable).toContainText('10.10.1.0/24');
    // VRF route must be strictly excluded from global RD view
    await expect(ribTable).not.toContainText('10.99.1.0/24');

    // Reset filters
    await vrfInput.fill('');
    await filterBtn.click();
  });

  test('5. Route attribute modification is reflected in browser DOM', async ({ page, request }) => {
    // Trigger route-map on frr2 via control server
    const modRes = await request.post(`${CTRL_URL}/frr/modify`);
    expect(modRes.status()).toBe(200);

    await page.goto(`${BONSAI_URL}/#/routing`);
    await page.waitForLoadState('domcontentloaded');

    const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
    const ribTable = page.locator('.panel', { hasText: 'Canonical RIB Explorer' }).locator('.data-table');

    // Poll Filter button until updated AS-path or MED is rendered
    await expect(async () => {
      await filterBtn.click();
      await expect(ribTable).toContainText('65002 65002', { timeout: 2000 });
    }).toPass({ timeout: 20_000, intervals: [1000] });

    // Also verify MED metric 150
    await expect(ribTable).toContainText('150');

    await page.screenshot({ path: path.join(EVIDENCE_DIR, '02_route_attribute_modified.png'), fullPage: true });
  });

  test('6. Route withdrawal removes prefix from active RIB table in browser DOM', async ({ page, request }) => {
    // Trigger route withdrawal on frr2 via control server
    const withRes = await request.post(`${CTRL_URL}/frr/withdraw`);
    expect(withRes.status()).toBe(200);

    await page.goto(`${BONSAI_URL}/#/routing`);
    await page.waitForLoadState('domcontentloaded');

    const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
    const ribPanel = page.locator('.panel', { hasText: 'Canonical RIB Explorer' });

    // Poll Filter button until 10.10.1.0/24 is removed from table
    await expect(async () => {
      await filterBtn.click();
      const text = await ribPanel.textContent();
      expect(text).not.toContain('10.10.1.0/24');
    }).toPass({ timeout: 20_000, intervals: [1000] });

    await page.screenshot({ path: path.join(EVIDENCE_DIR, '03_route_withdrawn.png'), fullPage: true });
  });

  test('7. Re-announcement and router-driven TCP EOF disconnect preserves routes and updates completeness to gap', async ({ page, request }) => {
    // Re-announce 10.10.1.0/24
    const reannRes = await request.post(`${CTRL_URL}/frr/announce`);
    expect(reannRes.status()).toBe(200);

    // Wait until route reappears
    await page.goto(`${BONSAI_URL}/#/routing`);
    const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
    const ribTable = page.locator('.panel', { hasText: 'Canonical RIB Explorer' }).locator('.data-table');

    await expect(async () => {
      await filterBtn.click();
      await expect(ribTable).toContainText('10.10.1.0/24', { timeout: 2000 });
    }).toPass({ timeout: 20_000, intervals: [1000] });

    // Trigger router-driven BMP TCP EOF disconnect while BGP session stays Up
    const discRes = await request.post(`${CTRL_URL}/bmp/disconnect`);
    expect(discRes.status()).toBe(200);

    // Poll Refresh until Header badge changes to gap
    const refreshBtn = page.locator('button.btn-refresh');
    const compBadge = page.locator('.workspace-header .badge', { hasText: /Completeness: gap/i });
    await expect(async () => {
      await refreshBtn.click();
      await expect(compBadge).toBeVisible({ timeout: 1000 });
    }).toPass({ timeout: 15_000, intervals: [1000] });

    // Data-quality notice alert for transport gap is displayed
    const gapAlert = page.locator('.alert.alert-warn', { hasText: /transport disconnect/i });
    await expect(gapAlert).toBeVisible();

    // CRUCIAL: Routes are PRESERVED in table with completeness badge 'gap' (no synthesized withdrawals)
    await expect(ribTable).toContainText('10.10.1.0/24');
    const rowGapBadge = ribTable.locator('.badge', { hasText: 'gap' });
    await expect(rowGapBadge.first()).toBeVisible();

    // Verify route count did NOT drop to 0 on TCP EOF
    const routesKpi = page.locator('.kpi-card', { hasText: 'Active RIB Routes' }).locator('.kpi-value');
    await expect(routesKpi).not.toHaveText('0');

    await page.screenshot({ path: path.join(EVIDENCE_DIR, '04_transport_gap_preservation.png'), fullPage: true });
  });

  test('8. Reconnect on session epoch 2 resolves transport gap and restores complete state', async ({ page, request }) => {
    // Enable and accept epoch 2 reconnect
    const reconRes = await request.post(`${CTRL_URL}/bmp/reconnect`);
    expect(reconRes.status()).toBe(200);

    await page.goto(`${BONSAI_URL}/#/routing`);
    const refreshBtn = page.locator('button.btn-refresh');

    // Poll until completeness updates to complete
    await expect(async () => {
      await refreshBtn.click();
      const badgeText = await page.locator('.workspace-header .badge', { hasText: /Completeness:/i }).textContent();
      expect(badgeText).toContain('complete');
      expect(badgeText).not.toContain('gap');
    }).toPass({ timeout: 20_000, intervals: [1000] });

    // Verify exporters table remains populated
    const exportersTable = page.locator('.panel', { hasText: 'BMP Exporters' }).locator('.data-table');
    await expect(exportersTable).toContainText('172.100.207.11');

    await page.screenshot({ path: path.join(EVIDENCE_DIR, '05_reconnect_epoch2.png'), fullPage: true });
  });

  test('9. Prefix History Timeline renders announcement, modification and withdrawal events with exact attributes', async ({ page }) => {
    await page.goto(`${BONSAI_URL}/#/routing`);
    await page.waitForLoadState('domcontentloaded');

    // Load Timeline for 10.10.1.0/24
    const historyInput = page.locator('input[aria-label="Prefix History Query"]');
    await historyInput.fill('10.10.1.0/24');
    const loadBtn = page.locator('button', { hasText: 'Load Timeline' });
    await loadBtn.click();

    const timelineTable = page.locator('.panel', { hasText: 'Prefix Event History Timeline' }).locator('.data-table');
    await expect(timelineTable).toBeVisible({ timeout: 10_000 });

    // Verify event badges: Announce and Withdraw
    await expect(timelineTable.locator('.badge', { hasText: 'Announce' }).first()).toBeVisible();
    await expect(timelineTable.locator('.badge', { hasText: 'Withdraw' }).first()).toBeVisible();

    // Verify before/after Path Attributes columns (NH, AS path before and prepend, MED)
    await expect(timelineTable).toContainText('NH: 10.100.1.1');
    await expect(timelineTable).toContainText('AS: 65002');
    await expect(timelineTable).toContainText('65002 65002');
    await expect(timelineTable).toContainText('MED: 150');

    await page.screenshot({ path: path.join(EVIDENCE_DIR, '06_prefix_history_timeline.png'), fullPage: true });
  });

  test('10. Truthful empty filter and unsupported family states are displayed accurately', async ({ page }) => {
    await page.goto(`${BONSAI_URL}/#/routing`);
    await page.waitForLoadState('domcontentloaded');

    // 1. Truthful empty state
    const pfxFilter = page.locator('input[aria-label="Prefix Filter"]');
    await pfxFilter.fill('198.51.100.99/32');
    const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
    await filterBtn.click();

    const ribPanel = page.locator('.panel', { hasText: 'Canonical RIB Explorer' });
    await expect(ribPanel).toContainText(/No routes match current filter/i, { timeout: 5000 });

    // 2. Truthful unsupported state
    await pfxFilter.fill('');
    const famSelect = page.locator('select[aria-label="Address Family"]');
    await famSelect.selectOption('bgp-ls');
    await filterBtn.click();

    await expect(page.locator('.alert.alert-warn')).toContainText(/unsupported/i);
    await expect(ribPanel).toContainText(/Address family '?bgp-ls'? has (support )?status/i);

    await page.screenshot({ path: path.join(EVIDENCE_DIR, '07_empty_and_unsupported_states.png'), fullPage: true });
  });

  test('11. Routing Service restart from durable journals preserves complete state', async ({ page, request }) => {
    // Reset address family filter back to ipv4-unicast
    await page.goto(`${BONSAI_URL}/#/routing`);
    const famSelect = page.locator('select[aria-label="Address Family"]');
    await famSelect.selectOption('ipv4-unicast');
    const filterBtn = page.locator('button', { hasText: 'Filter Routes' });
    await filterBtn.click();

    // Request restart of routing service from durable journals
    const restartRes = await request.post(`${CTRL_URL}/routing/restart`);
    expect(restartRes.status()).toBe(200);

    // Refresh UI and verify routes and exporters preserved from durable journals
    const refreshBtn = page.locator('button.btn-refresh');
    const ribTable = page.locator('.panel', { hasText: 'Canonical RIB Explorer' }).locator('.data-table');
    const exportersTable = page.locator('.panel', { hasText: 'BMP Exporters' }).locator('.data-table');

    await expect(async () => {
      await refreshBtn.click();
      await expect(ribTable).toContainText('10.10.1.0/24', { timeout: 2000 });
      await expect(ribTable).toContainText('10.99.1.0/24', { timeout: 2000 });
    }).toPass({ timeout: 20_000, intervals: [1000] });

    await expect(exportersTable).toContainText('172.100.207.11');
    await expect(exportersTable).toContainText('172.100.207.12');

    await page.screenshot({ path: path.join(EVIDENCE_DIR, '08_after_restart_preserved.png'), fullPage: true });
  });

  test('12. Logout revokes browser session and subsequent navigation is denied', async ({ page, request }) => {
    // Capture the pre-logout token before logout
    const cookiesBefore = await page.context().cookies(BONSAI_URL);
    const preLogoutToken = cookiesBefore.find(c => c.name === 'bonsai_token')?.value;
    expect(preLogoutToken).toBeTruthy();

    // Perform logout via page.request
    const logoutRes = await page.request.post(`${BONSAI_URL}/api/auth/logout`);
    expect(logoutRes.status()).toBe(204);

    // Verify Set-Cookie header cleared the token
    const cookiesAfter = await page.context().cookies(BONSAI_URL);
    const tokenCookie = cookiesAfter.find(c => c.name === 'bonsai_token');
    expect(!tokenCookie || tokenCookie.value === '' || tokenCookie.expires <= Date.now() / 1000).toBe(true);

    // Assert that replaying the saved pre-logout token is denied with HTTP 401 Unauthorized
    const replayRes = await request.get(`${BONSAI_URL}/api/routing/v1/health`, {
      headers: {
        'Authorization': `Bearer ${preLogoutToken}`
      }
    });
    expect(replayRes.status()).toBe(401);

    // Subsequent navigation to /#/routing renders 401 error
    await page.goto(`${BONSAI_URL}/#/routing`);
    await page.waitForLoadState('domcontentloaded');

    const alert = page.locator('.alert.alert-danger');
    await expect(alert).toBeVisible({ timeout: 10_000 });
    await expect(alert).toContainText(/401|Unavailable/i);

    await page.screenshot({ path: path.join(EVIDENCE_DIR, '09_logged_out_denied.png'), fullPage: true });
  });
});
