// @ts-check
import { test, expect } from '@playwright/test';

test.describe('Topology view', () => {
  test('loads without error', async ({ page }) => {
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));

    await page.goto('/');
    await page.waitForLoadState('networkidle');

    // SPA shell renders
    await expect(page.locator('body')).not.toBeEmpty();
    expect(errors).toHaveLength(0);
  });

  test('topology nav link is present', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    const nav = page.locator('nav a, aside a').filter({ hasText: /topology/i });
    await expect(nav.first()).toBeVisible();
  });

  test('topology page renders device nodes or empty state', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    // Either a D3 SVG or an empty-state message should be present
    const svg = page.locator('svg');
    const empty = page.locator('text=/no devices|empty|0 devices/i');
    await expect(svg.or(empty).first()).toBeVisible({ timeout: 10_000 });
  });
});
