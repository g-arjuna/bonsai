// D4-19 T4 — Live UI smoke tests (S-51)
// Validates that all major UI pages load, navigation works, and key elements render.
//
// Prerequisites:
//   npm install -D @playwright/test
//   npx playwright install chromium
//
// Run:
//   npx playwright test --config tests/playwright/playwright.config.js

const { test, expect } = require('@playwright/test');

// Helper: navigate and assert page loaded
async function assertPageLoads(page, path, expectedText) {
  await page.goto(path);
  await page.waitForLoadState('networkidle');
  if (expectedText) {
    await expect(page.locator('body')).toContainText(expectedText, { timeout: 10_000 });
  }
}

test.describe('Navigation smoke', () => {
  test('dashboard / live page loads', async ({ page }) => {
    await assertPageLoads(page, '/', 'Live');
  });

  test('incidents page loads', async ({ page }) => {
    await assertPageLoads(page, '/incidents', 'Incidents');
  });

  test('devices page loads', async ({ page }) => {
    await assertPageLoads(page, '/devices', 'Devices');
  });

  test('operations page loads', async ({ page }) => {
    await assertPageLoads(page, '/operations', 'Operations');
  });

  test('collectors page loads', async ({ page }) => {
    await assertPageLoads(page, '/collectors', 'Collectors');
  });

  test('integrations page loads', async ({ page }) => {
    await assertPageLoads(page, '/integrations', 'Integrations');
  });

  test('approvals page loads', async ({ page }) => {
    await assertPageLoads(page, '/approvals', 'Approvals');
  });

  test('explorer page loads', async ({ page }) => {
    await assertPageLoads(page, '/explorer', 'Explorer');
  });

  test('profiles page loads', async ({ page }) => {
    await assertPageLoads(page, '/profiles', 'Profiles');
  });

  test('config library page loads', async ({ page }) => {
    await assertPageLoads(page, '/config-library', 'Config Library');
  });

  test('settings page loads', async ({ page }) => {
    await assertPageLoads(page, '/settings', 'Settings');
  });

  test('sidecars page loads', async ({ page }) => {
    await assertPageLoads(page, '/sidecars', 'Sidecars');
  });

  test('syslog/shun page loads', async ({ page }) => {
    await assertPageLoads(page, '/syslog', 'Syslog');
  });

  test('governance page loads', async ({ page }) => {
    await assertPageLoads(page, '/governance', 'Governance');
  });

  test('database page loads', async ({ page }) => {
    await assertPageLoads(page, '/db', 'Database');
  });

  test('SNMP page loads', async ({ page }) => {
    await assertPageLoads(page, '/snmp', 'SNMP');
  });

  test('users page loads', async ({ page }) => {
    await assertPageLoads(page, '/users', 'Users');
  });

  test('audit page loads', async ({ page }) => {
    await assertPageLoads(page, '/audit', 'Audit');
  });
});

test.describe('Key interactions', () => {
  test('nav bar contains all primary links', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    const nav = page.locator('nav');
    await expect(nav).toBeVisible();
    // Check a subset of nav items
    for (const label of ['Live', 'Incidents', 'Devices', 'Collectors', 'Settings']) {
      await expect(nav.locator(`text=${label}`)).toBeVisible();
    }
  });

  test('clicking nav links navigates correctly', async ({ page }) => {
    await page.goto('/');
    await page.waitForLoadState('networkidle');
    await page.click('nav >> text=Incidents');
    await expect(page).toHaveURL(/\/incidents/);
    await page.click('nav >> text=Devices');
    await expect(page).toHaveURL(/\/devices/);
  });

  test('health API returns ok', async ({ request }) => {
    const response = await request.get('/health');
    expect(response.ok()).toBeTruthy();
    const body = await response.json();
    expect(body.status).toBe('ok');
  });

  test('setup status API returns valid shape', async ({ request }) => {
    const response = await request.get('/api/setup/status');
    expect(response.ok()).toBeTruthy();
    const body = await response.json();
    expect(body).toHaveProperty('is_first_run');
    expect(body).toHaveProperty('has_environments');
  });
});

test.describe('Config Library (D4-7 T4)', () => {
  test('config library page shows tabs', async ({ page }) => {
    await page.goto('/config-library');
    await page.waitForLoadState('networkidle');
    await expect(page.locator('text=Detection Rules')).toBeVisible();
    await expect(page.locator('text=gNMI Path Profiles')).toBeVisible();
    await expect(page.locator('text=Known Issues')).toBeVisible();
  });
});

test.describe('Sidecar Rules (D4-9 T4)', () => {
  test('sidecar page has rules tab', async ({ page }) => {
    await page.goto('/sidecars');
    await page.waitForLoadState('networkidle');
    await expect(page.locator('text=Rules')).toBeVisible();
  });
});
