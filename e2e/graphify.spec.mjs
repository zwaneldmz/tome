import { test, expect } from '@playwright/test'
import { openAddMenu } from './helpers.mjs'

// @graphify: the Code graph pane — one-click build against the mocked
// graphify bridge. The pane is workspace-gated, so every test seeds a
// workspaces store entry before boot (renderer.js restores wsState from
// it, which is what turns the ＋ menu item on). `seedFn` runs as an init
// script BEFORE goto, so a test can flip mock graphify state (available /
// built) before the pane ever probes it.
const WS = '/Users/test/demo'

async function bootWithWorkspace(page, seedFn) {
  await page.addInitScript({ path: 'e2e/tome-mock.js' })
  await page.addInitScript((ws) => {
    window.__tomeMock.store['workspaces'] = {
      workspaces: [{ name: 'demo', folders: [ws] }],
      active: 0,
    }
  }, WS)
  if (seedFn) await page.addInitScript(seedFn)
  await page.goto('/')
  await expect(page.locator('#btn-add')).toBeVisible()
}

async function openGraphifyPane(page) {
  await openAddMenu(page)
  await page.getByRole('menuitem', { name: /Code graph/ }).click()
  await expect(page.locator('.panel-graphify')).toBeVisible()
}

test.describe('@graphify code graph pane', () => {
  test('@smoke opening Code graph from the ＋ menu probes status for the active root', async ({
    page,
  }) => {
    await bootWithWorkspace(page)
    await openGraphifyPane(page)

    const calls = await page.evaluate(() => window.__tomeMock.calls.graphifyStatus)
    expect(calls).toEqual([WS])
    await expect(page.locator('.graphify-status')).toHaveText(/graphify 0\.9\.48/)
  })

  test('@smoke one-click build streams lines and unlocks the query bar', async ({ page }) => {
    await bootWithWorkspace(page)
    await openGraphifyPane(page)

    await page.locator('.graphify-build').click()
    // streamed lines land in the console
    await expect(page.locator('.graphify-console')).toContainText('[2/2] clustering')
    await expect(page.locator('.graphify-console')).toContainText('graph built')
    // after the build resolves, status re-probes and built flips true
    await expect(page.locator('.graphify-status')).toHaveText(/graph built/)
    await expect(page.locator('.graphify-open-graph')).toBeVisible()
    await expect(page.locator('.graphify-open-report')).toBeVisible()

    const builds = await page.evaluate(() => window.__tomeMock.calls.graphifyBuild)
    expect(builds).toEqual([WS])

    // the query bar is live now: ask something
    await page.locator('.graphify-input').fill('what connects the editor?')
    await page.locator('.graphify-run').click()
    await expect(page.locator('.graphify-result')).toHaveText('query result')
    const queries = await page.evaluate(() => window.__tomeMock.calls.graphifyQuery)
    expect(queries).toEqual([{ ws: WS, question: 'what connects the editor?' }])
  })

  test('@smoke a path query splits A → B and calls graphify.path', async ({ page }) => {
    // Pre-built graph: queries work without clicking Build.
    await bootWithWorkspace(page, () => {
      window.__tomeMock.graphify.built = true
    })
    await openGraphifyPane(page)

    await page.locator('.graphify-mode').selectOption('path')
    await page.locator('.graphify-input').fill('renderer.js → panes.js')
    await page.locator('.graphify-run').click()
    await expect(page.locator('.graphify-result')).toHaveText('path result')

    const calls = await page.evaluate(() => window.__tomeMock.calls.graphifyPath)
    expect(calls).toEqual([{ ws: WS, from: 'renderer.js', to: 'panes.js' }])
  })

  test('a missing graphify install disables Build and shows the install hint', async ({ page }) => {
    await bootWithWorkspace(page, () => {
      window.__tomeMock.graphify.available = false
    })
    await openGraphifyPane(page)

    await expect(page.locator('.graphify-status')).toHaveText('graphify not installed')
    await expect(page.locator('.graphify-build')).toBeDisabled()
    await expect(page.locator('.graphify-run')).toBeDisabled()
    await expect(page.locator('.graphify-console')).toContainText('pipx install graphifyy')
  })
})
