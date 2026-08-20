import { expect } from '@playwright/test'

// Injects the window.tome mock and boots the renderer to a ready grid.
export async function boot(page) {
  await page.addInitScript({ path: 'e2e/tome-mock.js' })
  await page.goto('/')
  await expect(page.locator('#btn-add')).toBeVisible()
}

// Opens the topbar ＋ menu (populated async from the mocked agents.list()).
export async function openAddMenu(page) {
  await page.click('#btn-add')
  await expect(page.locator('#add-menu')).toBeVisible()
  // The agent rows are appended after tome.agents.list() resolves.
  await expect(page.getByRole('menuitem', { name: /claude/ })).toBeVisible()
}

// Reads the recorded pty.create calls from the injected mock.
export function ptyCreateCalls(page) {
  return page.evaluate(() => window.__tomeMock.calls.ptyCreate)
}
