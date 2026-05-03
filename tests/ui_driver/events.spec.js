// @ts-check
import { test, expect } from '@playwright/test';

test.describe('Events view', () => {
  test('events page renders', async ({ page }) => {
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));

    await page.goto('/events');
    await page.waitForLoadState('networkidle');

    // Page should have a heading
    const heading = page.locator('h2').first();
    await expect(heading).toBeVisible();
    expect(errors).toHaveLength(0);
  });

  test('SSE connection is attempted', async ({ page }) => {
    const eventSourceRequests = [];
    page.on('request', (req) => {
      if (req.url().includes('/api/events')) {
        eventSourceRequests.push(req.url());
      }
    });

    await page.goto('/events');
    await page.waitForLoadState('networkidle');

    // Give SSE a moment to connect
    await page.waitForTimeout(2_000);
    expect(eventSourceRequests.length).toBeGreaterThan(0);
  });
});
