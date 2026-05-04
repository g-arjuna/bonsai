// @ts-check
// T4-4: Per-route screenshot capture and diff.
//
// Tagged @screenshot so the CI screenshot-diff workflow can run these
// selectively with --grep "@screenshot".
//
// Baselines are stored in screenshots/baseline/.
// Run `npx playwright test --update-snapshots` to refresh them.
import { test, expect } from '@playwright/test';

const ROUTES = [
  { name: 'topology',     path: '/' },
  { name: 'incidents',    path: '/incidents' },
  { name: 'collectors',   path: '/collectors' },
  { name: 'operations',   path: '/operations' },
  { name: 'devices',      path: '/devices' },
  { name: 'environments', path: '/environments' },
  { name: 'enrichment',   path: '/enrichment' },
  { name: 'adapters',     path: '/adapters' },
];

for (const { name, path } of ROUTES) {
  test(`@screenshot ${name} workspace renders consistently`, async ({ page }) => {
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));

    await page.goto(path);
    // Wait for networkidle so data fetch + SSE setup have completed.
    await page.waitForLoadState('networkidle');
    // Extra settle time for SSE connection indicator and data rendering.
    await page.waitForTimeout(1_500);

    // No JS errors allowed.
    expect(errors).toHaveLength(0);

    // Screenshot comparison against committed baseline.
    await expect(page).toHaveScreenshot(`${name}.png`, {
      maxDiffPixelRatio: 0.02,  // 2% pixel variance allowed for antialiasing differences
      animations: 'disabled',
    });
  });
}
