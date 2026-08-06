# <img src="docs/icon.png" width="28" align="top" alt=""> Tome

A neon-on-black desktop coding harness: your agents, terminals, editors,
documents, and an assistant — one workspace, one grid.

![Tome](docs/screenshot.png)

## What it does

- **Workspaces** — named groups of project folders. The `▚` chip in the top
  bar names the active workspace; click it to switch, create, or add folders.
  The tree shows every folder in the workspace; whatever you click becomes the
  *active root* that new panes and the git widget follow.
- **Agent panes** — the `＋` menu spawns Claude Code, opencode, or pi in a
  real PTY (login shell, your prompt, your keybindings). Agents light up
  automatically when their CLI appears on `PATH` — no config.
- **Pane grid** — dockview tiling: drag to rearrange, drop one pane onto
  another to stack as named tabs.
- **Editor** — CodeMirror 6, language auto-detect, `⌘S` saves, dirty-dot in
  the tab.
- **Documents** — PDFs open in Chromium's viewer, images inline, `.docx` and
  `.xlsx` are converted and rendered in sandboxed frames; anything else falls
  back to "Open in default app".
- **Git** — clickable branch chip (switch or create branches, IntelliJ-style)
  with live working-tree counters `+added ~modified −deleted` and `↑↓`
  ahead/behind.
- **Assistant chat** — streams Claude (`claude-opus-5`, override with
  `TOME_CHAT_MODEL`) from the main process; the API key never enters the
  renderer.
- **Air gap (macOS)** — agent panes spawn inside a seatbelt sandbox that kills
  all direct network egress (DNS included); the only way out is a per-pane
  local proxy that allows **model-provider domains only**. The cyan strip on
  the pane frees it — passphrase (plus optional authenticator-app 2FA), scoped
  to that pane, auto-relocking after 15/30/60 minutes. Blocked hosts surface
  on the strip and as toasts.

## Air gap notes

- Unlocking widens the *proxy*, never the sandbox: freed panes get HTTP(S)
  through the proxy; raw sockets/ssh never work inside an air-gapped pane —
  spawn an unrestricted pane for that (toggle in the ＋ menu).
- Claude Code's WebSearch/WebFetch are server-side (they run at
  api.anthropic.com), so air-gapped claude can still search; opencode/pi
  client-side fetch is genuinely blocked until freed.
- Tools that ignore proxy env vars fail *closed* — they get nothing.
- Allowlist lives in `~/Library/Application Support/tome/airgap.json`
  (loaded at launch; edits apply on restart). The auth file is unreadable and
  the config unwritable from inside sandboxed panes.

## Run

```bash
npm install        # rebuilds node-pty for Electron's ABI (needs Xcode CLT)
npm run dev
```

If `npm run dev` fails with `Error: Electron uninstall`, a script-blocking
npm guard prevented Electron's binary download — run `npm run fix:electron`
once.

The assistant pane needs `ANTHROPIC_API_KEY` in the environment (or an
`ant auth login` profile). Without credentials the pane shows a setup hint;
everything else works.

Set `TOME_SHOT=/tmp/shot.png npm run dev` to boot into demo panes and write a
screenshot — handy for design passes.

## Install as an app

```bash
npm run icon       # regenerate the sprite icon (edit the grid in scripts/gen-icon.mjs)
npm run package    # → dist/mac-arm64/Tome.app (unsigned; ad-hoc sign if you like)
ditto dist/mac-arm64/Tome.app /Applications/Tome.app
```

The icon is a hand-authored 16×16 pixel sprite (a neon grimoire) rendered to
every macOS size by `scripts/gen-icon.mjs` — no image tooling required.

## Stack

Electron + electron-vite · dockview-core (pane grid) · @xterm/xterm +
node-pty (real PTYs) · CodeMirror 6 · mammoth + SheetJS (documents) ·
@anthropic-ai/sdk (assistant).

## License

[MIT](LICENSE)
