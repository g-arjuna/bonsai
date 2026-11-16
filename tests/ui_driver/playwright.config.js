// @ts-check
import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: '**/*.spec.js',
  timeout: 30_000,
  retries: 1,
  reporter: [
    ['list'],
    ['json', { outputFile: '../../runtime/driver_results/ui.json' }],
  ],
  use: {
    baseURL: process.env.BONSAI_URL ?? 'http://localhost:3000',
    headless: true,
    screenshot: 'only-on-failure',
    video: 'off',
    // Only override the browser executable when explicitly requested. The
    // previous hardcoded chromium-1228 fallback pinned a browser build that
    // no longer matches the @playwright/test version in package-lock.json,
    // and bypassed PLAYWRIGHT_BROWSERS_PATH resolution entirely. Without an
    // override, playwright resolves the chromium build provisioned for the
    // pinned driver version.
    ...(process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH
      ? {
          launchOptions: {
            executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH,
          },
        }
      : {}),
  },
});
