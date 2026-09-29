// The v1 shell: palette, sidebar (Open / Changes), seal, welcome.
//@feature @shell @smoke
import { test, expect } from '@playwright/test'
import { boot, ptyCreateCalls, runCommand } from './helpers.mjs'

const WS = '/Users/test/demo'
// Runs as boot()'s seed, i.e. after the mock has built window.tome.
const withRepo = () => {
  window.__tomeMock.store.workspaces = { workspaces: [{ name: 'demo', folders: ['/Users/test/demo'] }], active: 0 }
  const t = window.tome
  t.git.info = async () => ({ repo: true, branch: 'main', added: 1, modified: 1, deleted: 0, ahead: 0, behind: 0 })
  t.git.status = async () => ({ files: [{ x: ' ', y: 'M', path: 'src/app.js' }, { x: '?', y: '?', path: 'notes.md' }] })
  window.__diffCalls = []
  t.git.diff = async (dir, hash, file) => {
    window.__diffCalls.push([dir, hash, file])
    return '--- a/src/app.js\n+++ b/src/app.js\n@@ -1 +1 @@\n-old line\n+new line'
  }
}

test.describe('@shell', () => {
  test('the palette starts an agent, sealed', async ({ page }) => {
    await boot(page)
    await runCommand(page, 'New claude agent')
    const calls = await ptyCreateCalls(page)
    expect(calls).toHaveLength(1)
    expect(calls[0]).toMatchObject({ kind: 'claude', egress: true })
    // …and it shows up in Open, with its seal
    const row = page.locator('#side-open .open-row')
    await expect(row).toHaveCount(1)
    await expect(row).toContainText('claude')
    await expect(row.locator('.seal-tag')).toHaveText('sealed')
  })

  test('the welcome surface starts agents with one click', async ({ page }) => {
    await boot(page, withRepo)
    await page.locator('.welcome-card', { hasText: 'opencode' }).click()
    expect((await ptyCreateCalls(page))[0].kind).toBe('opencode')
  })

  test('Changes lists the working tree and opens a file diff against HEAD', async ({ page }) => {
    await boot(page, withRepo)
    await page.click('#side-tabs [data-view=changes]')
    const rows = page.locator('.change-row')
    await expect(rows).toHaveCount(2)
    await expect(rows.first().locator('.change-code')).toHaveText('M')
    await expect(rows.nth(1).locator('.change-code')).toHaveText('U')
    await rows.first().click()
    await expect(page.locator('.panel-diff .dl.add')).toHaveText('+new line')
    expect(await page.evaluate(() => window.__diffCalls)).toEqual([[WS, '', 'src/app.js']])
    // commit stays disabled until there is a message
    const commit = page.locator('.commit-actions .btn-primary')
    await expect(commit).toBeDisabled()
    await page.fill('.commit-msg', 'fix the thing')
    await expect(commit).toBeEnabled()
  })

  test('the seal flips "start agents sealed" and the next agent follows it', async ({ page }) => {
    await boot(page)
    await expect(page.locator('#seal-text')).toHaveText('Sealed')
    await page.click('#seal-chip')
    const sw = page.getByRole('menuitemcheckbox', { name: /Start agents sealed/ })
    await expect(sw).toHaveAttribute('aria-checked', 'true')
    await sw.click()
    await expect(sw).toHaveAttribute('aria-checked', 'false')
    await expect(page.locator('#seal-text')).toHaveText('Unsealed')
    expect(await page.evaluate(() => window.__tomeMock.store['egress-default'])).toBe(false)
    await page.keyboard.press('Escape')
    await runCommand(page, 'New claude agent')
    expect((await ptyCreateCalls(page))[0].egress).toBe(false)
  })

  test('palette ">" filters to commands; Esc closes and nothing runs', async ({ page }) => {
    await boot(page)
    await page.keyboard.press('ControlOrMeta+k')
    await page.keyboard.type('>theme')
    await expect(page.locator('.pal-row').first()).toContainText('Theme:')
    await page.keyboard.press('Escape')
    await expect(page.locator('#palette')).toHaveCount(0)
  })
})
