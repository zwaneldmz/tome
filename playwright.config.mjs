import { defineConfig, devices } from '@playwright/test'

// Tome renderer E2E — drives the Vite-built renderer in headless Chromium
// against the `window.tome` mock (e2e/tome-mock.js). Fast, deterministic,
// and CI-portable: no Tauri binary, no WebDriver, no display server needed.
//
// Tags: every spec carries a `@feature` tag (e.g. @docker, @panes,
// @preferences) plus a `@smoke` tag on the fast happy-path suites. Run a
// single feature with:
//
//     bunx playwright test --grep @docker
//
// Run only the fast suites in CI with:
//
//     bunx playwright test --grep @smoke
export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL: 'http://localhost:5199',
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'bunx vite --port 5199 --strictPort',
    url: 'http://localhost:5199',
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
})
