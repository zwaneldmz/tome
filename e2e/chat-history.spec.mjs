import { test, expect } from '@playwright/test'
import { openAddMenu } from './helpers.mjs'

// @chat-history: searchable conversation archive + fresh start per
// workspace startup + assistant-root sync. The fresh-start spec captures a
// REAL dockview layout in-page (hand-writing the toJSON shape would just
// pin an invented schema), then replays restoreLayout() exactly as boot
// does.

const WS = '/Users/test/demo'

async function bootSeeded(page, seed) {
  await page.addInitScript({ path: 'e2e/tome-mock.js' })
  await page.addInitScript(([ws, seed]) => {
    window.__tomeMock.store['workspaces'] = {
      workspaces: [{ name: 'demo', folders: [ws] }],
      active: 0,
    }
    // An old conversation that belongs to the PREVIOUS session's pane.
    window.__tomeMock.store['chat-log-chat-1'] = [
      { role: 'user', content: 'old question from last session' },
      { role: 'assistant', content: 'old answer' },
    ]
    if (seed) seed(window.__tomeMock.store)
  }, [WS, seed])
  await page.goto('/')
  await expect(page.locator('#btn-add')).toBeVisible()
}

test.describe('@chat-history history + fresh start', () => {
  test('@smoke a workspace startup restores the chat pane FRESH — the old transcript stays in the archive', async ({ page }) => {
    await bootSeeded(page)
    // Open the assistant; its transcript restores from the store (2 msgs).
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /Assistant chat/ }).click()
    const pane = page.locator('.panel-chat')
    await expect(pane).toBeVisible()
    await expect(pane.locator('.chat-log .msg')).toHaveCount(2)

    // Capture the REAL saved-layout shape, exactly as boot would have it,
    // then replay restoreLayout() — the previous session "ended" here.
    await page.evaluate(async () => {
      const mod = await import('/panes.js')
      const saved = mod.dock.toJSON()
      const layoutKey =
        'layout-' +
        '/Users/test/demo'
          .replace(/[^a-z0-9-]+/g, '-')
          .replace(/^-+|-+$/g, '')
          .slice(0, 90)
      window.__tomeMock.store[layoutKey] = saved
      await mod.restoreLayout()
    })

    // The restored pane is CLEAR — the old conversation did not come back.
    const restored = page.locator('.panel-chat')
    await expect(restored).toBeVisible()
    await expect(restored.locator('.chat-log .msg')).toHaveCount(0)
    // …and the archive still holds the old transcript for search.
    const stored = await page.evaluate(() => window.__tomeMock.store['chat-log-chat-1'])
    expect(stored).toHaveLength(2)
  })

  test('@smoke the assistant root syncs to the ACTIVE workspace folder at boot', async ({ page }) => {
    await bootSeeded(page)
    const roots = await page.evaluate(() => window.__tomeMock.calls.conductorSetCwd)
    expect(roots).toEqual([WS])
  })

  test('@smoke history search lists, filters, and re-anchors the pane to a past conversation', async ({ page }) => {
    await bootSeeded(page)
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /Assistant chat/ }).click()
    const pane = page.locator('.panel-chat')
    await expect(pane).toBeVisible()

    // seed the archive; the modal lists it newest-first
    await page.evaluate(() => {
      window.__tomeMock.history = [
        { id: 'chat-7', count: 4, snippet: 'why is auth failing', mtimeMs: 300 },
        { id: 'chat-3', count: 2, snippet: 'release notes please', mtimeMs: 200 },
      ]
    })
    await pane.locator('.chat-history-btn').click()
    await expect(page.locator('#ag-overlay')).toBeVisible()
    await expect(page.locator('.hist-row')).toHaveCount(2)
    await expect(page.locator('.hist-row').first()).toContainText('why is auth failing')

    // typing searches server-side (the mock records the query; the empty
    // string is the modal's initial unfiltered listing)
    await page.locator('.hist-search').fill('release')
    const queries = await page.evaluate(() => window.__tomeMock.calls.chatHistoryList)
    expect(queries).toEqual(['', 'release'])

    // picking re-anchors: modal closes, transcript renders, and a new turn
    // continues THAT log
    await page.evaluate(() => {
      window.__tomeMock.store['chat-log-chat-3'] = [
        { role: 'user', content: 'release notes please' },
        { role: 'assistant', content: 'here you go' },
      ]
      window.__tomeMock.history = [{ id: 'chat-3', count: 2, snippet: 'release notes please', mtimeMs: 200 }]
      // P3.1: a send requires a picked provider — seed one, or the consent
      // gate would open the picker instead of sending this turn.
      window.__tomeMock.store['chat-provider'] = 'kimi'
      window.tome.chat.providers = async () => ({
        providers: [],
        active: 'kimi',
        effective: {
          id: 'kimi',
          label: 'Kimi (Moonshot)',
          model: 'kimi-k3',
          host: 'https://api.moonshot.ai/v1',
          keyOrigin: { kind: 'file' },
        },
        reason: null,
        none: false,
      })
    })
    await page.locator('.hist-search').fill('release')
    await page.locator('.hist-row').first().click()
    await expect(page.locator('#ag-overlay')).toHaveCount(0)
    await expect(pane.locator('.chat-log .msg')).toHaveCount(2)
    await expect(pane.locator('.chat-log')).toContainText('release notes please')

    await pane.locator('textarea').fill('make it shorter')
    await pane.locator('textarea').press('Enter')
    await expect(pane.locator('.chat-log .msg.me')).toHaveCount(2)
    await page.waitForTimeout(500) // persistHistory's 400ms debounce
    const stored = await page.evaluate(() => window.__tomeMock.store['chat-log-chat-3'])
    expect(stored).toHaveLength(3)
  })
})
