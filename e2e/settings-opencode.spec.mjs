import { test, expect } from '@playwright/test'
import { openAddMenu } from './helpers.mjs'

// @settings: the Settings overlay's close affordance (WIG: visible ✕, not
// Escape/backdrop-only) and the opencode section (credentials, default
// model, login handoff). The overlay is app-wide — no workspace needed.
// `seedFn` runs as an init script AFTER the tome mock and BEFORE goto.

async function openSettings(page, seedFn) {
  await page.addInitScript({ path: 'e2e/tome-mock.js' })
  if (seedFn) await page.addInitScript(seedFn)
  await page.goto('/')
  await expect(page.locator('#btn-add')).toBeVisible()
  await openAddMenu(page)
  await page.getByRole('menuitem', { name: /Settings/ }).click()
  await expect(page.locator('.prefs-shell')).toBeVisible()
}

test.describe('@settings settings overlay', () => {
  test('@smoke every modal has a labelled close button that closes the overlay', async ({ page }) => {
    await openSettings(page)

    const close = page.locator('#ag-overlay .ag-close')
    await expect(close).toBeVisible()
    await expect(close).toHaveAttribute('aria-label', 'Close dialog')
    await close.click()
    await expect(page.locator('#ag-overlay')).toHaveCount(0)
  })

  test('@smoke opencode section lists credentials and saves a new key', async ({ page }) => {
    await openSettings(page)

    // The section mounts async under the Agents group.
    await expect(page.locator('[data-section="opencode"] h4')).toHaveText('opencode')

    // Existing credential row: type only, never the key.
    await expect(page.locator('[data-section="opencode"]')).toContainText('deepseek')
    await expect(page.locator('[data-section="opencode"]')).toContainText('API key set')

    // Save a replacement key for an existing provider.
    await page
      .locator('[data-section="opencode"] .prefs-row', { hasText: 'deepseek' })
      .locator('input[type="password"]')
      .fill('sk-new')
    await page
      .locator('[data-section="opencode"] .prefs-row', { hasText: 'deepseek' })
      .getByRole('button', { name: 'Save' })
      .click()

    const calls = await page.evaluate(() => window.__tomeMock.calls.opencodeKeySet)
    expect(calls).toEqual([{ provider: 'deepseek', key: 'sk-new' }])
  })

  test('@smoke opencode default model select saves and the login button opens a terminal pane', async ({ page }) => {
    await openSettings(page, () => {
      window.__tomeMock.calls.opencodeModels = ['deepseek/deepseek-chat', 'eurouter/glm-5.2']
    })

    const sel = page.locator('[data-section="opencode"] select[aria-label="opencode default model"]')
    await expect(sel).toBeVisible()
    await sel.selectOption('deepseek/deepseek-chat')
    const models = await page.evaluate(() => window.__tomeMock.calls.opencodeSetModel)
    expect(models).toEqual(['deepseek/deepseek-chat'])

    // Login closes Settings and spawns a terminal pane with the login
    // command as its initial command.
    await page
      .locator('[data-section="opencode"]')
      .getByRole('button', { name: /Log in/ })
      .click()
    await expect(page.locator('#ag-overlay')).toHaveCount(0)
    const ptys = await page.evaluate(() => window.__tomeMock.calls.ptyCreate)
    expect(ptys).toHaveLength(1)
    expect(ptys[0].kind).toBe('terminal')
    expect(ptys[0].cmd).toBe('opencode providers login')
  })

  test('a missing opencode install shows the hint and no credentials form', async ({ page }) => {
    await openSettings(page, () => {
      window.__tomeMock.opencode = {
        installed: false,
        version: null,
        reason: 'opencode not found (No such file or directory (os error 2))',
        auth: [],
        providers: [],
        providers_with_key: [],
        default_model: null,
      }
    })
    await expect(page.locator('[data-section="opencode"]')).toContainText('opencode is not installed')
  })
})
