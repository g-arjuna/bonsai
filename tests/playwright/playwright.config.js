// D4-19 T4 — Playwright smoke test configuration
// Usage: npx playwright test --config tests/playwright/playwright.config.js

/** @type {import('@playwright/test').PlaywrightTestConfig} */
const config = {
  testDir: '.',
  timeout: 30_000,
  retries: 1,
  use: {
    baseURL: process.env.BONSAI_URL || 'http://localhost:3000',
    headless: true,
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
    trace: 'retain-on-failure',
  },
  outputDir: '../../runtime/playwright-results',
  reporter: [
    ['list'],
    ['json', { outputFile: '../../runtime/playwright-results/results.json' }],
  ],
  projects: [
    { name: 'chromium', use: { browserName: 'chromium' } },
  ],
};

module.exports = config;
