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

  // P1.2 rung-2 honesty: a Linux pane whose Landlock file confinement
  // failed open is reported as network-contained only, and the strip
  // SAYS so — visible text, not a tooltip.
  test('@containment a network-only pane says so on its egress strip', async ({ page }) => {
    await boot(page)
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /opencode/ }).click()

    // Drive the push the real backend sends: the pane (first spawn =
    // pty-1, gapped because egress-default is on in the mock store)
    // reports confinement: 'network-only'.
    await page.evaluate(() => {
      window.__tomeMock.emit('egress:state', {
        panes: {
          'pty-1': { mode: 'providers', expiresAt: null, confinement: 'network-only' },
        },
        defaultMinutes: 15,
        repo: [],
        auth: { configured: false, totp: false },
      })
    })

    await expect(page.locator('.egress-strip .ag-label')).toContainText(
      'network-contained only'
    )
  })

  // P2.1 containment-only ceiling: with the pref on, the ＋ menu drops the
  // unsandboxed Terminal row and the egress-default toggle (a default, not
  // a ceiling), and agent rows stay — forced gapped.
  test('@containment containment-only removes unsandboxed spawns from the ＋ menu', async ({
    page,
  }) => {
    await boot(page, () => {
      window.__tomeMock.store['containment-only'] = true
      // Off-by-default: the ceiling must force agents gapped even when the
      // egress-default DEFAULT is off.
      window.__tomeMock.store['egress-default'] = false
    })
    await openAddMenu(page)

    await expect(page.getByRole('menuitem', { name: /Terminal/ })).toHaveCount(0)
    await expect(page.getByRole('menuitem', { name: /spawn agents contained/ })).toHaveCount(0)
    // Agent rows survive — and are marked contained.
    await expect(page.getByRole('menuitem', { name: /⛨ claude/ })).toBeVisible()

    // Spawning one still records a GAPPED pty.create: the ceiling forces
    // egress on even though the menu no longer offers the toggle.
    await page.getByRole('menuitem', { name: /⛨ claude/ }).click()
    const calls = await ptyCreateCalls(page)
    expect(calls).toHaveLength(1)
    expect(calls[0].kind).toBe('claude')
    expect(calls[0].egress).toBe(true)
  })
})
