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
    launchOptions: {
      executablePath:
        process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH ||
        `${process.env.HOME}/.cache/ms-playwright/chromium-1228/chrome-linux64/chrome`,
    },
  },
});
