import { test, expect } from '@playwright/test'
import { boot, openAddMenu } from './helpers.mjs'

// @plan-tracker: the execution HUD. Fires the exact event sequence the
// backend emits for a real orchestrated turn (chat:tool → chat:tool-done
// per step, conductor:agent for headless runs, chat:done at the end) at
// the mock's handler registry, and watches the HUD project it.

const fire = (page, event, payload) => page.evaluate(([e, p]) => window.__tomeMock.emit(e, p), [event, payload])

test.describe('@plan-tracker execution HUD', () => {
  test('@smoke a tool call opens the HUD; completion checks the step and finishes cleanly', async ({ page }) => {
    await boot(page)

    // nothing before execution starts
    await expect(page.locator('.plan-hud')).toHaveCount(0)

    // step 1 begins
    await fire(page, 'chat:tool', { id: 'chat-1', tool: 'run_command', hint: 'cargo test' })
    const hud = page.locator('.plan-hud')
    await expect(hud).toBeVisible()
    await expect(hud.locator('.plan-title')).toHaveText('executing plan')
    await expect(hud.locator('.plan-step')).toHaveCount(1)
    await expect(hud.locator('.plan-step.active .plan-tool')).toHaveText(/run_command/)
    await expect(hud.locator('.plan-step.active .plan-hint')).toHaveText(/cargo test/)

    // step 1 completes, step 2 begins (a headless agent delegation)
    await fire(page, 'chat:tool-done', { id: 'chat-1', tool: 'run_command', hint: 'cargo test', ok: true, ms: 1240 })
    await fire(page, 'chat:tool', { id: 'chat-1', tool: 'run_agent', hint: 'claude' })
    await expect(hud.locator('.plan-step')).toHaveCount(2)
    await expect(hud.locator('.plan-step.ok')).toHaveCount(1)
    await expect(hud.locator('.plan-step.ok .plan-ms')).toHaveText('1.2s')

    // the agent's lifecycle shows as a live sub-row on its step
    await fire(page, 'conductor:agent', { chatId: 'chat-1', kind: 'claude', status: 'started' })
    await expect(hud.locator('.plan-agent.running')).toHaveText(/claude — running/)
    await fire(page, 'conductor:agent', { chatId: 'chat-1', kind: 'claude', status: 'done' })
    await expect(hud.locator('.plan-agent.done')).toHaveText(/claude — done/)

    // turn ends clean: step 2 completes, the HUD finishes and fades
    await fire(page, 'chat:tool-done', { id: 'chat-1', tool: 'run_agent', hint: 'claude', ok: true, ms: 4200 })
    await fire(page, 'chat:done', { id: 'chat-1', error: null, aborted: false })
    await expect(hud.locator('.plan-title')).toHaveText(/plan complete/)
    await expect(hud.locator('.plan-fill')).toHaveAttribute('style', /100%/)
    await expect(hud).toHaveClass(/plan-hud-hidden/, { timeout: 6000 })
  })

  test('@smoke a refused step turns red, the plan reads as failed, and the ✕ dismisses it', async ({ page }) => {
    await boot(page)

    await fire(page, 'chat:tool', { id: 'chat-9', tool: 'run_command', hint: 'rm -rf /' })
    await fire(page, 'chat:tool-done', { id: 'chat-9', tool: 'run_command', hint: 'rm -rf /', ok: false, ms: 3 })
    const hud = page.locator('.plan-hud')
    await expect(hud.locator('.plan-step.fail .plan-dot')).toHaveText('✕')
    await expect(hud.locator('.plan-fill')).toHaveClass(/plan-fill-fail/)

    await fire(page, 'chat:done', { id: 'chat-9', error: 'boom', aborted: false })
    await expect(hud.locator('.plan-title')).toHaveText('plan failed')
    await expect(hud).toHaveClass(/plan-hud-fail/)

    // dismiss: gone now, and the ✕ is labelled (WIG)
    const close = hud.locator('.plan-close')
    await expect(close).toHaveAttribute('aria-label', 'Dismiss plan tracker')
    await close.click()
    await expect(hud).toHaveClass(/plan-hud-hidden/)
  })

  test('a new turn resets the steps — the HUD never shows a finished run again', async ({ page }) => {
    await boot(page)

    // turn 1 completes and hides
    await fire(page, 'chat:tool', { id: 'chat-1', tool: 'list_panes', hint: '' })
    await fire(page, 'chat:tool-done', { id: 'chat-1', tool: 'list_panes', hint: '', ok: true, ms: 5 })
    await fire(page, 'chat:done', { id: 'chat-1', error: null, aborted: false })
    await expect(page.locator('.plan-hud')).toHaveClass(/plan-hud-hidden/)

    // turn 2 opens fresh: one step, not three
    await fire(page, 'chat:tool', { id: 'chat-1', tool: 'read_file', hint: 'src/lib.rs' })
    const hud = page.locator('.plan-hud')
    await expect(hud).not.toHaveClass(/plan-hud-hidden/)
    await expect(hud.locator('.plan-step')).toHaveCount(1)
    await expect(hud.locator('.plan-step.active .plan-tool')).toHaveText(/read_file/)
  })

  test('clicking the header focuses the chat pane that is executing', async ({ page }) => {
    await boot(page)
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /Assistant chat/ }).click()
    await expect(page.locator('.panel-chat')).toBeVisible()

    await fire(page, 'chat:tool', { id: 'chat-1', tool: 'list_panes', hint: '' })
    const hud = page.locator('.plan-hud')
    await expect(hud).toBeVisible()

    // blur the chat (open a terminal) then click the HUD header: the chat
    // pane becomes active again
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /^Terminal/ }).click()
    await expect(page.locator('.panel-terminal').first()).toBeVisible()
    await hud.locator('.plan-head').click({ position: { x: 60, y: 10 } })
    await expect(page.locator('.panel-chat')).toBeVisible()
  })
})
