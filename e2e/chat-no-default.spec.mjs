// @chat: no default assistant provider (launch hardening P3.1). A fresh
// profile makes ZERO provider requests until a row is picked: the first
// send in a chat pane opens the provider picker (Preferences → Assistant)
// instead of calling chat.send, the header says "No provider — pick one",
// and once a pick exists the send flows through the ordinary path.
import { test, expect } from '@playwright/test'
import { boot, openAddMenu } from './helpers.mjs'

const openChat = async (page) => {
  await openAddMenu(page)
  await page.getByRole('menuitem', { name: /Assistant chat/ }).click()
  const pane = page.locator('.panel-chat')
  await expect(pane).toBeVisible()
  return pane
}

test.describe('@chat no default assistant provider', () => {
  test('@smoke fresh profile: first send opens the picker and makes no request', async ({ page }) => {
    await boot(page) // the mock store has no chat-provider — a fresh install
    const pane = await openChat(page)

    // The header names the consent gap, not a provider.
    await expect(pane.locator('.chat-provider-line')).toHaveText('No provider — pick one')

    await pane.locator('textarea').fill('why is the sky blue')
    await pane.locator('textarea').press('Enter')

    // The picker (deep-linked to Assistant), not a request — and no send
    // crossed the bridge at all.
    await expect(page.locator('.prefs-shell')).toBeVisible()
    await expect(page.locator('[data-section="assistant"]')).toBeVisible()
    const sends = await page.evaluate(() => window.__tomeMock.calls.chatSend)
    expect(sends).toHaveLength(0)

    // The draft survives and no user bubble painted — the pane never
    // entered a send, so the pick costs nothing.
    await expect(pane.locator('textarea')).toHaveValue('why is the sky blue')
    await expect(pane.locator('.chat-log .msg')).toHaveCount(0)
  })

  test('@smoke picked provider: the send flows and the pick sticks', async ({ page }) => {
    await boot(page)
    // A profile where the user picked kimi (store seeded) — the header
    // shows the real resolution and the send reaches the bridge.
    await page.evaluate(() => {
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
    const pane = await openChat(page)
    await expect(pane.locator('.chat-provider-line')).toHaveText(/Kimi \(Moonshot\) · kimi-k3/)

    await pane.locator('textarea').fill('hello')
    await pane.locator('textarea').press('Enter')

    // The ordinary send path: one chat.send, no picker popup.
    await expect
      .poll(() => page.evaluate(() => window.__tomeMock.calls.chatSend.length))
      .toBe(1)
    await expect(page.locator('.prefs-shell')).toHaveCount(0)
  })
})
