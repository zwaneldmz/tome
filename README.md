# Tome

A desktop coding harness: a project browser on the left, a grid of panes for
everything else — Claude Code, opencode, plain terminals, an assistant chat,
and code editors — opened from one `＋` button, arranged how you like,
collapsed to a title bar when you need the space.

## Run

```bash
npm install        # rebuilds node-pty for Electron's ABI (needs Xcode CLT)
npm run dev
```

If `npm run dev` fails with `Error: Electron uninstall`, the allow-scripts
guard blocked Electron's binary download during install — run
`npm run fix:electron` once.

The assistant chat pane needs `ANTHROPIC_API_KEY` in the environment
(model override: `TOME_CHAT_MODEL`, default `claude-sonnet-5`). Without a key
the pane shows a setup hint; everything else works.

Agent entries in the `＋` menu (claude / opencode / pi) light up automatically
when the CLI is on `PATH` — no config file.

## Stack

Electron + electron-vite · dockview-core (pane grid, drag-to-rearrange,
tab-stacking) · @xterm/xterm + node-pty (real PTYs) · CodeMirror 6 (editor) ·
Anthropic Messages API (chat, streamed from the main process).

## Deliberately deferred

git-worktree awareness · chat markdown + tool use · LSP · pane-layout
persistence · packaging/distribution · pi (until installed).
