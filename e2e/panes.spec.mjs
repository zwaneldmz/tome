import { test, expect } from '@playwright/test'
import { boot, openAddMenu, ptyCreateCalls } from './helpers.mjs'

// @panes: pane-spawn wiring through the ＋ menu. Exemplar suite showing the
// tag convention — each app feature gets its own tagged spec file.
test.describe('@panes pane spawning', () => {
  test('@smoke spawning an agent records a gapped pty.create', async ({ page }) => {
    await boot(page)
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /opencode/ }).click()

    const calls = await ptyCreateCalls(page)
    expect(calls).toHaveLength(1)
    expect(calls[0].kind).toBe('opencode')
    expect(calls[0].egress).toBe(true) // egress-default is on
  })

  test('@smoke spawning a plain terminal records kind terminal and no gap', async ({ page }) => {
    await boot(page)
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /Terminal/ }).click()

    const calls = await ptyCreateCalls(page)
    expect(calls).toHaveLength(1)
    expect(calls[0].kind).toBe('terminal')
  })
})
