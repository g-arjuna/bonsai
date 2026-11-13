// @ts-check
// BN-015-U1 browser acceptance: NetBox onboarding must key selection on
// the stable external identity (never the IP), keep ineligible candidates
// unselectable, import as inventory-only, and show per-object results —
// including an independent conflict for two NetBox devices sharing one IP.
// Requires NETBOX_MOCK_URL (served by tests/bn015_u1_inventory_browser.rs).
import { test, expect } from '@playwright/test';

test.describe.configure({ retries: 0 }); // import is not idempotent across retries
test.describe('NetBox inventory onboarding', () => {
  test('preview, stable-identity selection, import results, inventory-only', async ({ page }) => {
    test.skip(!process.env.NETBOX_MOCK_URL, 'requires mock NetBox (NETBOX_MOCK_URL)');
    test.setTimeout(90_000);
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));

    await page.goto('/#/devices/new');
    await page.waitForLoadState('domcontentloaded');

    // Switch to the NetBox import workspace.
    await page.getByRole('button', { name: 'Import from NetBox' }).click();
    await expect(page.locator('#nb-url')).toBeVisible({ timeout: 15_000 });

    await page.fill('#nb-url', process.env.NETBOX_MOCK_URL);
    await page.fill('#nb-token', 'browser-acceptance-token');
    await page.getByRole('button', { name: 'Fetch devices' }).click();

    // Version + warnings render; the ineligible no-IP device is surfaced.
    await expect(page.locator('text=/NetBox.*4\\.2\\.1/')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.warning').filter({ hasText: /noip-device/ })).toBeVisible();
    await expect(page.locator('.warning').filter({ hasText: /no primary IP/ })).toBeVisible();

    // Two DIFFERENT NetBox objects share address 10.30.5.2 — both rows
    // must render independently (stable identity, not address-keyed).
    const dupA = page.locator('.nb-candidate-row', { hasText: 'dup-a' });
    const dupB = page.locator('.nb-candidate-row', { hasText: 'dup-b' });
    await expect(dupA).toBeVisible();
    await expect(dupB).toBeVisible();
    await expect(dupA.locator('code')).toHaveText('10.30.5.2');
    await expect(dupB.locator('code')).toHaveText('10.30.5.2');

    // The no-IP candidate is ineligible: disabled checkbox + reason shown.
    const noip = page.locator('.nb-candidate-row', { hasText: 'noip-device' });
    await expect(noip.locator('input[type=checkbox]')).toBeDisabled();
    await expect(noip).toContainText('no primary IP address');

    // Deselect everything, then select exactly the four interesting rows
    // independently — proves selection is per-object, not per-address.
    const selectAll = page.locator('.select-all input[type=checkbox]');
    await selectAll.click(); // clears the default all-eligible selection
    const wanted = ['edge-r1', 'dup-a', 'dup-b', 'v6-edge'];
    for (const name of wanted) {
      const row = page.locator('.nb-candidate-row', { hasText: name });
      await row.locator('input[type=checkbox]').check();
    }
    await expect(page.locator('text=/4 of .* eligible selected/')).toBeVisible();

    // Import — inventory-only path, no credential prompts anywhere.
    await page.getByRole('button', { name: /Import 4 devices as inventory only/ }).click();

    // Per-object results keyed by external identity:
    //  - edge-r1 imported → "inventory only"
    //  - dup-a imported (first canonical-address claim wins)
    //  - dup-b conflicts on the same canonical address → "conflict"
    //  - v6-edge imported (IPv6 identity normalizes cleanly)
    //  - noip-device untouched (was never selectable)
    await expect(page.locator('.nb-candidate-row', { hasText: 'edge-r1' }).locator('.badge', { hasText: 'inventory only' })).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('.nb-candidate-row', { hasText: 'dup-a' }).locator('.badge', { hasText: 'inventory only' })).toBeVisible();
    await expect(page.locator('.nb-candidate-row', { hasText: 'dup-b' }).locator('.badge', { hasText: /conflict/ })).toBeVisible();
    await expect(page.locator('.nb-candidate-row', { hasText: 'v6-edge' }).locator('.badge', { hasText: 'inventory only' })).toBeVisible();
    await expect(page.locator('.nb-candidate-row', { hasText: 'noip-device' }).locator('.badge')).toHaveCount(0);

    // The UI states the lifecycle contract explicitly.
    await expect(page.locator('text=/inventory only.*no credentials/i')).toBeVisible();

    // Devices page: imported assets carry the inventory-only badge and
    // no credential alias was assigned.
    await page.goto('/#/devices');
    await page.waitForLoadState('domcontentloaded');
    const devices = await page.evaluate(async () => {
      const r = await fetch('/api/onboarding/devices');
      return r.json();
    });
    const byAddr = Object.fromEntries((devices.devices || []).map((d) => [d.address, d]));
    expect(byAddr['10.30.5.1']?.inventory_only).toBe(true);
    expect(byAddr['10.30.5.1']?.enabled).toBe(false);
    expect(byAddr['10.30.5.1']?.credential_alias || '').toBe('');
    expect(byAddr['10.30.5.2']?.inventory_only).toBe(true);
    expect(byAddr['2001:db8::10']?.inventory_only).toBe(true);
    // Stable external identity round-trips to the API.
    expect(byAddr['10.30.5.1']?.external_id).toMatch(/^netbox:https?:[^ ]+:device:8$/);
    // Inventory-declared metadata persisted, missing status stays empty.
    expect(byAddr['10.30.5.1']?.inventory_platform).toBe('srlinux');
    expect(byAddr['10.30.5.1']?.inventory_status).toBe('active');

    expect(errors).toHaveLength(0);
  });
});
