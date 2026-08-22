// Settings shell (slice 3a): two-pane shell, parallel builds with
// placeholders, live search, rail sync, deep links, and return-to-place
// after nested flows. Tagged @preferences; fast (@smoke) — mock-backed.
//@feature @preferences @smoke
import { test, expect } from '@playwright/test'
import { boot, openAddMenu } from './helpers.mjs'

const openSettings = async (page) => {
  await boot(page)
  await openAddMenu(page)
  await page.getByRole('menuitem', { name: /Settings/ }).click()
  await expect(page.locator('.prefs-shell')).toBeVisible()
}

test.describe('slice 3a settings shell', () => {
  test('paints instantly with placeholders, fills in parallel, pane order preserved', async ({ page }) => {
    await boot(page)
    // Slow every async probe — the modal must still paint at once.
    await page.evaluate(() => {
      const slow = (v) => () => new Promise((res) => setTimeout(() => res(v), 1200))
      window.tome.chat.providers = slow({ providers: [], active: null })
      const origGet = window.tome.store.get
      window.tome.store.get = async (key) => {
        await new Promise((r) => setTimeout(r, 1200))
        return origGet(key)
      }
      window.tome.agents.customs = slow([])
      window.tome.agents.list = slow([
        { name: 'claude', available: true, custom: false },
        { name: 'opencode', available: true, custom: false },
      ])
      window.tome.exportDest.list = slow([])
      window.tome.schedules.list = slow([])
      window.tome.remote.sources = slow([])
      window.tome.stt.status = slow({ available: false })
      window.tome.stt.engine = slow('apple')
    })
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /Settings/ }).click()

    // Immediately: shell + rail + sync sections + loading placeholders.
    await expect(page.locator('.prefs-nav')).toBeVisible()
    await expect(page.locator('.prefs-pane')).toBeVisible()
    await expect(page.locator('[data-section="appearance"]')).toBeVisible()
    await expect(page.locator('[data-section="security"]')).toBeVisible()
    await expect(page.locator('[data-section="mentor"]')).toBeVisible()
    await expect(page.getByText('Loading…')).toHaveCount(7)
    await expect(page.locator('.prefs-nav-item[data-group]')).toHaveCount(7)
    await expect(page.getByRole('button', { name: 'Replay setup wizard…' })).toBeVisible()

    // …then the slow sections fill in, each in its own slot.
    await expect(page.locator('[data-section="assistant"] .prefs-row').first()).toBeVisible({
      timeout: 5000,
    })
    await expect(page.locator('[data-section="agents"] .prefs-row').first()).toBeVisible({
      timeout: 5000,
    })
    await expect(page.getByText('Loading…')).toHaveCount(0)
    const ids = await page.locator('.prefs-pane [data-section]').evaluateAll((ns) =>
      ns.map((n) => n.dataset.section)
    )
    expect(ids).toEqual([
      'appearance',
      'terminal',
      'editor',
      'sidebar',
      'assistant',
      'custom-provider',
      'agents',
      'security',
      'export',
      'schedules',
      'remote',
      'voice',
      'mentor',
    ])
  })

  test('live search: filters rows, hides empty sections, dims groups, Esc restores', async ({ page }) => {
    await openSettings(page)
    await expect(page.locator('[data-section="security"] .prefs-row')).toHaveCount(4)
    const search = page.locator('.prefs-search')
    await search.fill('2fa')

    // Only Security's Two-factor row survives.
    await expect(page.locator('.prefs-row:not(.prefs-row-hidden)')).toHaveCount(1)
    await expect(page.locator('.prefs-row:not(.prefs-row-hidden)')).toHaveText(
      /Two-factor authentication/
    )
    await expect(page.locator('.prefs-section:not(.prefs-section-hidden)')).toHaveCount(1)
    await expect(page.locator('.prefs-nav-item[data-group="security"]')).not.toHaveClass(/prefs-nav-dim/)
    await expect(page.locator('.prefs-nav-item[data-group="mentor"]')).toHaveClass(/prefs-nav-dim/)
    await expect(page.locator('.prefs-match-count')).toBeVisible()
    await expect(page.locator('.prefs-match-count')).toHaveText(/1 of \d+ sections/)

    // Esc clears and restores everything.
    await search.press('Escape')
    await expect(page.locator('.prefs-section-hidden')).toHaveCount(0)
    await expect(page.locator('.prefs-row-hidden')).toHaveCount(0)
    await expect(page.locator('.prefs-match-count')).toBeHidden()
    await expect(page.locator('.prefs-nav-dim')).toHaveCount(0)
    // Esc again (no query) closes the modal as before.
    await search.press('Escape')
    await expect(page.locator('#ag-overlay')).toHaveCount(0)
  })

  test('search indexes hint text, placeholders, and non-row content', async ({ page }) => {
    await openSettings(page)
    await page.locator('.prefs-search').fill('key')
    // assistant: the login-shell keys hint + the "● key found" row hint;
    // custom-provider: the "API key" placeholder + the 0600-store hint.
    const visible = await page
      .locator('.prefs-section:not(.prefs-section-hidden)')
      .evaluateAll((ns) => ns.map((n) => n.dataset.section))
    expect(visible.sort()).toEqual(['assistant', 'custom-provider'])
  })

  test('rail click scrolls; scroll sync sets aria-current; "/" focuses search', async ({ page }) => {
    await openSettings(page)
    await expect(page.locator('.prefs-nav-item[data-group="general"]')).toHaveAttribute(
      'aria-current',
      'true'
    )
    await page.locator('.prefs-nav-item[data-group="mentor"]').click()
    await expect(page.locator('.prefs-nav-item[data-group="mentor"]')).toHaveAttribute(
      'aria-current',
      'true',
      { timeout: 5000 }
    )
    // '/' from a focused button jumps to the search box.
    await page.locator('.prefs-nav-item[data-group="mentor"]').focus()
    await page.keyboard.press('/')
    await expect(page.locator('.prefs-search')).toBeFocused()
  })

  test('deep link: section or group id scrolls and flashes', async ({ page }) => {
    await openSettings(page)
    await page.evaluate(async () => {
      const m = await import('/preferences.js')
      m.preferencesModal({ section: 'voice' })
    })
    await expect(page.locator('[data-section="voice"]')).toHaveClass(/prefs-highlight/)
    // group id works too
    await page.evaluate(async () => {
      const m = await import('/preferences.js')
      m.preferencesModal({ section: 'integrations' })
    })
    await expect(page.locator('[data-section="export"]')).toHaveClass(/prefs-highlight/)
  })

  test('return-to-place: enroll authenticator reopens Settings at Security', async ({ page }) => {
    await openSettings(page)
    await page.getByRole('button', { name: 'Enroll authenticator (2FA)…' }).click()
    // totpModal took the overlay's place
    await expect(page.getByRole('dialog', { name: /enroll authenticator/i })).toBeVisible()
    await expect(page.locator('.prefs-shell')).toHaveCount(0)
    // cancel the nested flow — Settings must come back at Security
    await page.keyboard.press('Escape')
    await expect(page.locator('.prefs-shell')).toBeVisible({ timeout: 3000 })
    await expect(page.locator('.prefs-nav-item[data-group="security"]')).toHaveAttribute(
      'aria-current',
      'true',
      { timeout: 3000 }
    )
  })

  test('return-to-place: replay setup wizard reopens Settings at General', async ({ page }) => {
    await openSettings(page)
    await page.getByRole('button', { name: 'Replay setup wizard…' }).click()
    // wizard mounts after its async auth probe
    await expect(page.getByRole('dialog', { name: /Set up Tome/ })).toBeVisible({ timeout: 3000 })
    await page.keyboard.press('Escape') // not dirty → closes
    await expect(page.locator('.prefs-shell')).toBeVisible({ timeout: 3000 })
    await expect(page.locator('.prefs-nav-item[data-group="general"]')).toHaveAttribute(
      'aria-current',
      'true',
      { timeout: 3000 }
    )
  })

  test('return-to-place: Add destination… (integrations, through the confirm chain)', async ({ page }) => {
    await openSettings(page)
    // export section is async — the add button auto-waits for it
    await page.getByRole('button', { name: 'Add destination…' }).click()
    await expect(page.getByRole('dialog', { name: 'Add export destination' })).toBeVisible()
    await page.getByPlaceholder('e.g. Staging bucket').fill('Staging')
    await page.getByPlaceholder('https://example.com/uploads').fill('https://x.example/upload')
    await page.getByRole('button', { name: 'Continue' }).click()
    await expect(page.getByRole('dialog', { name: 'Add export destination' })).toBeVisible()
    await page.getByRole('button', { name: 'Add', exact: true }).click()
    // chain over → Settings reopens at Integrations
    await expect(page.locator('.prefs-shell')).toBeVisible({ timeout: 3000 })
    await expect(page.locator('.prefs-nav-item[data-group="integrations"]')).toHaveAttribute(
      'aria-current',
      'true',
      { timeout: 3000 }
    )
  })

  test('narrow viewport: rail collapses, search and wizard action stay', async ({ page }) => {
    await page.setViewportSize({ width: 640, height: 800 })
    await openSettings(page)
    await expect(page.locator('.prefs-search')).toBeVisible()
    await expect(page.locator('.prefs-nav-item[data-group="general"]')).toBeHidden()
    await expect(page.getByRole('button', { name: 'Replay setup wizard…' })).toBeVisible()
    // still searchable in collapsed mode
    await page.locator('.prefs-search').fill('2fa')
    await expect(page.locator('.prefs-row:not(.prefs-row-hidden)')).toHaveCount(1)
  })
})
