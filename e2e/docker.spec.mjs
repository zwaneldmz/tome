import { test, expect } from '@playwright/test'
import { boot, openAddMenu, ptyCreateCalls } from './helpers.mjs'

// Sandboxed Docker: the filtered-gateway opt-in is OFF by default, gated by
// BOTH a global master (Preferences → Security → "Allow sandboxed Docker")
// and a per-pane spawn toggle (＋ menu → "sandboxed docker"). This suite
// pins the renderer half of that contract: the toggles actually thread
// `docker` through to `tome.pty.create`. The backend half (the gateway
// filter) is covered by the Rust suite + the live docker_gateway_live test.
test.describe('@docker sandboxed docker', () => {
  test('@smoke per-pane toggle is disabled until the global master is on', async ({ page }) => {
    await boot(page)
    await openAddMenu(page)
    const item = page.getByRole('menuitem', { name: /sandboxed docker/ })
    await expect(item).toBeVisible()
    await expect(item).toBeDisabled()
  })

  test('spawns an agent with docker only when both toggles are on', async ({ page }) => {
    await boot(page)

    // 1. Global master ON via Preferences.
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /Settings/ }).click()
    const row = page.locator('.prefs-row').filter({ hasText: 'Allow sandboxed Docker' })
    await row.getByRole('switch').click()
    await expect(row.getByRole('switch')).toHaveAttribute('aria-checked', 'true')
    await page.keyboard.press('Escape')

    // 2. Per-pane spawn toggle ON (this click closes the menu, like every
    // menu-item click does).
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /sandboxed docker/ }).click()

    // 3. Spawn an agent pane.
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /claude/ }).click()

    const calls = await ptyCreateCalls(page)
    expect(calls).toHaveLength(1)
    expect(calls[0].docker).toBe(true)
    expect(calls[0].egress).toBe(true)
  })

  test('leaves docker absent when the per-pane toggle is off', async ({ page }) => {
    await boot(page)
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /claude/ }).click()

    const calls = await ptyCreateCalls(page)
    expect(calls).toHaveLength(1)
    expect(calls[0].docker).toBeUndefined()
  })
})
