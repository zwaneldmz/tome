import { defineConfig } from 'vitest/config'

// Agent worktrees under .claude/worktrees are full project checkouts with
// their own test/ trees — running them from here double-runs stale copies of
// suites that already pass in test/ (and fails when a worktree predates a
// main-tree fix).
export default defineConfig({
  test: {
    // e2e/ holds Playwright specs — vitest's default include pattern would
    // sweep them up and fail at import ("test.describe() called here").
    exclude: ['**/node_modules/**', '**/.claude/**', '**/.worktrees/**', 'e2e/**'],
  },
})
