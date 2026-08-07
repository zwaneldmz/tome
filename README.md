# <img src="docs/icon.png" width="28" align="top" alt=""> Tome

A desktop coding harness: your agents, terminals, editors, documents, and an
assistant — one workspace, one grid. Light and dark, and it follows the
system by default.

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
  another to stack as named tabs. Every pane header carries a `＋` that opens
  a new pane *as a tab in that group* — so an agent and the helpers it needs
  stay stacked together instead of carving up the grid. The assistant's own
  `open_pane` follows the same rule: what it opens lands as a tab beside the
  chat that asked for it.
- **Panes as windows** — the `⧉` in a pane header tears that group off into
  its own OS window, and so does dragging a pane past the edge of the window
  (onto a second display, say — it opens where you dropped it). Closing the
  window docks the panes back into the grid, and the arrangement is saved
  with the rest of the layout.
- **Appearance** — light, dark, or match the system, from the `◐` in the top
  bar. Terminals, editors, the note graph, and converted documents all
  re-skin live. `⌘B` folds the sidebar away.
- **Editor** — CodeMirror 6, language auto-detect, `⌘S` saves, dirty-dot in
  the tab.
- **Documents** — PDFs open in Chromium's viewer, images inline, `.docx` and
  `.xlsx` are converted and rendered in sandboxed frames; anything else falls
  back to "Open in default app".
- **Git** — clickable branch chip (switch or create branches, IntelliJ-style)
  with live working-tree counters `+added ~modified −deleted` and `↑↓`
  ahead/behind. The git menu's **History** opens an IntelliJ-style log pane:
  commit list with ref chips and filter, commit message + changed files, and
  a per-file diff view.
- **Assistant chat** — streams Claude **Opus 4.8** through the Requesty router
  by default (`anthropic/claude-opus-4-8` via `REQUESTY_API_KEY`); with no
  Requesty key it falls back to direct Anthropic (`claude-opus-5` via
  `ANTHROPIC_API_KEY`). Override with `TOME_CHAT_MODEL` / `TOME_CHAT_BASE_URL`.
  Streaming runs in the main process; the API key never enters the renderer. The assistant is also the workspace **conductor**: it can list
  panes, read a terminal's scrollback, type into terminals, and open panes and
  files — so you can ask it "what is claude doing in the other pane?" or "run
  the tests over there". It only ever *submits* a command when the ＋ menu
  toggle **assistant may run commands** is on (default off); otherwise typed
  commands wait for your Enter. Toggle `🔊` to have replies spoken aloud
  (macOS voices); dictate into the box with the macOS mic key (`🎤` / double-Fn).
- **App login** — once a passphrase is set, Tome locks at launch: unlock with
  Touch ID or passphrase (+ authenticator code when enrolled). The gate is
  enforced in the main process — pty, fs, git, chat, and brain IPC all refuse
  until login — not just painted over. First run offers setup (skippable).
- **Air gap (macOS)** — agent panes spawn inside a seatbelt sandbox that kills
  all direct network egress (DNS included); the only way out is a per-pane
  local proxy that allows **model-provider domains only**. The cyan strip on
  the pane frees it — scoped to that pane, auto-relocking after 15/30/60
  minutes. Because the app login already proved your passphrase, freeing a
  pane asks only for the second factor: the authenticator code when 2FA is
  enrolled, the passphrase otherwise. Blocked hosts surface on the strip and
  as toasts.

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

The assistant pane needs `REQUESTY_API_KEY` (router, default) or
`ANTHROPIC_API_KEY` (direct) in the environment (or an `ant auth login`
profile). Without credentials the pane shows a setup hint; everything else
works.

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
