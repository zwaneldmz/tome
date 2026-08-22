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
    await expect(page.getByText('Loading…')).toHaveCount(6)
    await expect(page.locator('.prefs-nav-item[data-group]')).toHaveCount(7)
    await expect(page.getByRole('button', { name: 'Replay setup wizard…' })).toBeVisible()

    // …then the slow sections fill in, each in its own slot. The assistant
    // section renders provider cards (the mock has none) — its banner is
    // the first visible child either way.
    await expect(page.locator('[data-section="assistant"] .prefs-hint').first()).toBeVisible({
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
      'agents',
      'opencode',
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
    // assistant: the provider cards' key fields and key hints; opencode:
    // the credential rows' key inputs and 'API key set' hints.
    const visible = await page
      .locator('.prefs-section:not(.prefs-section-hidden)')
      .evaluateAll((ns) => ns.map((n) => n.dataset.section))
    expect(visible).toEqual(['assistant', 'opencode'])
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

  test('assistant cards: render, save key, hide, rejection line, search, egress jump, add provider', async ({ page }) => {
    await boot(page)
    await page.evaluate(() => {
      const keys = {}
      const hidden = []
      window.tome.chat.providers = async () => ({
        providers: [
          {
            id: 'glm', label: 'GLM (Z.ai)', model: 'glm-5.3',
            models: ['glm-5.3', 'glm-5.2'], baseUrl: 'https://api.z.ai/api/paas/v4',
            alternates: [{ label: 'China (Zhipu)', baseUrl: 'https://open.bigmodel.cn/api/paas/v4' }],
            keyEnv: null, keySet: false, keyOrigin: null, active: false,
            agentEgress: 'not-allowlisted', lastError: null, builtin: true,
          },
          {
            id: 'myai', label: 'My AI', model: 'm1', models: ['m1'],
            baseUrl: 'https://myai.example.com/v1', alternates: [],
            keyEnv: null, keySet: false, keyOrigin: null, active: true,
            agentEgress: 'not-allowlisted',
            lastError: 'Chat credentials rejected — check the My AI key (⌘, → Assistant) and try again.',
            builtin: false,
          },
        ],
        active: 'myai',
        effective: { id: 'myai', label: 'My AI', model: 'm1', host: 'https://myai.example.com/v1', keyOrigin: null },
        reason: null,
      })
      window.tome.chat.keySet = async (id, key) => {
        keys[id] = key
        return {}
      }
      window.tome.chat.providerSet = async (id, patch) => {
        if (patch?.hidden) hidden.push(id)
        return {}
      }
      window.tome.chat.providerDelete = async () => ({})
      window.tome.chat.providerAdd = async (label) => ({ id: label.toLowerCase() })
      window.__cardkeys = keys
      window.__cardhidden = hidden
    })
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /Settings/ }).click()

    // Cards render; the banner carries the REAL resolution.
    await expect(page.locator('.prov-card')).toHaveCount(2)
    await expect(page.locator('[data-section="assistant"] .prefs-hint').first()).toHaveText(/Next message → My AI/)

    // The rejection line (delta 2) renders on the row that failed.
    const myai = page.locator('.prov-card').filter({ hasText: 'My AI' })
    await expect(myai.getByText('⛔ Chat credentials rejected')).toBeVisible()

    // Egress gap jumps to Security (checklist 14).
    await myai.getByRole('button', { name: 'Security → Egress' }).click()
    await expect(page.locator('.prefs-nav-item[data-group="security"]')).toHaveAttribute('aria-current', 'true', { timeout: 5000 })

    // Write-only key save (delta 3): inbound command, field clears.
    const glm = page.locator('.prov-card').filter({ hasText: 'GLM (Z.ai)' })
    await glm.getByPlaceholder('paste API key').fill('z-key-123')
    await glm.getByRole('button', { name: 'Save key' }).click()
    await expect.poll(() => page.evaluate(() => window.__cardkeys.glm)).toBe('z-key-123')

    // Hide a built-in (delta 6).
    await glm.getByRole('button', { name: 'Hide' }).click()
    await expect.poll(() => page.evaluate(() => window.__cardhidden.includes('glm'))).toBe(true)

    // Model ids are indexed by the live search (checklist 13).
    await page.locator('.prefs-search').fill('glm-5.2')
    await expect(page.locator('.prefs-section:not(.prefs-section-hidden)')).toHaveCount(1)
    await expect(page.locator('.prov-card').filter({ hasText: 'GLM (Z.ai)' })).toBeVisible()
    await page.locator('.prefs-search').fill('')

    // + Add provider writes through providerAdd + keySet and selects the row.
    await page.locator('[data-section="assistant"]').getByRole('button', { name: '+ Add provider' }).click()
    await page.getByPlaceholder('label — e.g. Groq').fill('Groq')
    await page.getByPlaceholder('base URL — e.g. https://api.groq.com/openai/v1').fill('https://api.groq.com/openai/v1')
    await page.getByPlaceholder('model id — e.g. llama-4').fill('llama-4')
    await page.locator('[data-section="assistant"]').getByPlaceholder('API key', { exact: true }).fill('gsk-x')
    await page.locator('[data-section="assistant"]').getByRole('button', { name: 'Add', exact: true }).click()
    await expect.poll(() => page.evaluate(() => window.__tomeMock.store['chat-provider'])).toBe('groq')
    await expect.poll(() => page.evaluate(() => window.__cardkeys.groq)).toBe('gsk-x')
  })
})
