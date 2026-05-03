// @ts-check
import { test, expect } from '@playwright/test';

test.describe('Operations view', () => {
  test('loads operations page', async ({ page }) => {
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));

    await page.goto('/operations');
    await page.waitForLoadState('networkidle');

    await expect(page.locator('h2').filter({ hasText: /operations/i })).toBeVisible();
    expect(errors).toHaveLength(0);
  });

  test('shows metric cards', async ({ page }) => {
    await page.goto('/operations');
    await page.waitForLoadState('networkidle');

    // Wait for the skeleton loaders to resolve
    await page.waitForSelector('.card.metric', { timeout: 8_000 });
    const cards = page.locator('.card.metric');
    await expect(cards.first()).toBeVisible();
    expect(await cards.count()).toBeGreaterThanOrEqual(3);
  });

  test('memory and disk cards present', async ({ page }) => {
    await page.goto('/operations');
    await page.waitForLoadState('networkidle');
    await page.waitForSelector('.card.metric', { timeout: 8_000 });

    const rss = page.locator('.card.metric').filter({ hasText: /rss memory/i });
    const archive = page.locator('.card.metric').filter({ hasText: /archive on disk/i });
    const graph = page.locator('.card.metric').filter({ hasText: /graph db on disk/i });

    await expect(rss).toBeVisible();
    await expect(archive).toBeVisible();
    await expect(graph).toBeVisible();
  });

  test('prometheus metrics link is present', async ({ page }) => {
    await page.goto('/operations');
    await page.waitForLoadState('networkidle');
    const link = page.locator('a[href="/metrics"]');
    await expect(link).toBeVisible();
  });
});
