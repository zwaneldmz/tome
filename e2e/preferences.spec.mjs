import { test, expect } from '@playwright/test'
import { boot, openAddMenu } from './helpers.mjs'

// @preferences: the Settings modal writes store keys and flips prefs.
test.describe('@preferences settings modal', () => {
  test('@smoke toggling "Assistant may run commands" persists to the store', async ({ page }) => {
    await boot(page)
    await openAddMenu(page)
    await page.getByRole('menuitem', { name: /Settings/ }).click()

    const row = page.locator('.prefs-row').filter({ hasText: 'Assistant may run commands' })
    await row.getByRole('switch').click()
    await expect(row.getByRole('switch')).toHaveAttribute('aria-checked', 'true')

    const stored = await page.evaluate(() => window.__tomeMock.store['conductor-run'])
    expect(stored).toBe(true)
  })
})
