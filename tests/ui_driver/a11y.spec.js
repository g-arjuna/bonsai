// @ts-check
import { test, expect } from '@playwright/test';
import { injectAxe, checkA11y } from 'axe-playwright';

test.describe('Accessibility audit', () => {
  const routes = [
    '/',
    '/devices',
    '/topology',
    '/approvals',
    '/enrichment',
    '/adapters',
    '/operations',
    '/setup'
  ];

  for (const route of routes) {
    test(`route ${route} should be accessible`, async ({ page }) => {
      await page.goto(route);
      await page.waitForLoadState('networkidle');
      
      // Inject axe-core
      await injectAxe(page);
      
      // Run accessibility check
      // We use a relaxed check first to see what we have
      await checkA11y(page, null, {
        detailedReport: true,
        detailedReportOptions: { html: true }
      });
    });
  }
});
