# <img src="docs/icon.png" width="28" align="top" alt=""> Tome

**Run your coding agents behind an air gap, in one workspace.** Agents,
terminals, editors, documents, and an assistant in one grid — light and dark,
following the system by default.

<!-- The tests badge is static — bump the count by hand when the suite grows. -->
[![CI](https://github.com/zwaneldmz/tome/actions/workflows/build.yml/badge.svg)](https://github.com/zwaneldmz/tome/actions/workflows/build.yml)
[![tests: 181 passing](https://img.shields.io/badge/tests-181_passing-brightgreen)](test)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

![Tome](docs/screenshot.png)

<!-- Requires GitHub Pages enabled on the zwaneldmz/tome mirror (owner action). -->
New here? [How Tome works — an interactive tour](https://zwaneldmz.github.io/tome/how-tome-works.html)

## The air gap

Agent panes spawn inside a seatbelt sandbox that kills all direct network
egress (DNS included); the only way out is a per-pane CONNECT proxy on
`127.0.0.1` that allows **model-provider domains only**. The cyan strip on the
pane frees it — scoped to that pane, auto-relocking after 15/30/60 minutes.
Because the app login already proved your passphrase, freeing a pane asks only
for the second factor: the authenticator code when 2FA is enrolled, the
passphrase otherwise. Blocked hosts surface on the strip and as toasts.

- Unlocking widens the *proxy*, never the sandbox: freed panes get HTTP(S)
  through the proxy; raw sockets/ssh never work inside an air-gapped pane —
  spawn an unrestricted pane for that (toggle in the ＋ menu).
- Tools that ignore proxy env vars fail *closed* — they get nothing.
- The allowlist lives in `~/Library/Application Support/tome/airgap.json`
  (loaded at launch; edits apply on restart). The auth file is unreadable and
  the config unwritable from inside sandboxed panes.
- A repo can commit a **team allowlist** at `.tome/airgap.json`. It is
  validated, and the user must consent before any of it is honored — consent
  is pinned to the file's hash and verified in the main process, so a
  post-consent edit re-prompts. Starter sets and the org story:
  [docs/policy-presets.md](docs/policy-presets.md).
- A persistent **security event log** (`userData/events.jsonl`) records
  conductor tool calls, unlocks/relocks, and blocked egress hosts — actions,
  never payloads. Read it in the app via the **Event log** pane in the ＋
  menu.
- Claude Code's WebSearch/WebFetch are server-side (they run at
  api.anthropic.com), so air-gapped claude can still search; opencode/pi
  client-side fetch is genuinely blocked until freed.

## Install

Dev run:

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

Packaged app:

```bash
npm run icon       # regenerate the sprite icon (edit the grid in scripts/gen-icon.mjs)
npm run package    # → dist/mac-arm64/Tome.app
ditto dist/mac-arm64/Tome.app /Applications/Tome.app
```

Builds are currently **unsigned** — signed/notarized releases are on the
roadmap. Until then, after copying to `/Applications`:

```bash
xattr -dr com.apple.quarantine /Applications/Tome.app
```

(or right-click → Open once).

The icon is a hand-authored 16×16 pixel sprite (a neon grimoire) rendered to
every macOS size by `scripts/gen-icon.mjs` — no image tooling required.

Set `TOME_SHOT=/tmp/shot.png npm run dev` to boot into demo panes and write a
screenshot — handy for design passes (dev mode only, never packaged).

## The conductor

The assistant chat streams Claude **Opus 4.8** through the Requesty router by
default (`anthropic/claude-opus-4-8` via `REQUESTY_API_KEY`); with no Requesty
key it falls back to direct Anthropic (`claude-opus-5` via
`ANTHROPIC_API_KEY`). Override with `TOME_CHAT_MODEL` / `TOME_CHAT_BASE_URL`.
Streaming runs in the main process; the API key never enters the renderer.

The assistant is also the workspace **conductor**: a tool loop that can list
panes, read a terminal's scrollback, type into terminals, and open panes and
files — so you can ask it "what is claude doing in the other pane?" or "run
the tests over there". It only ever *submits* a command when the ＋ menu
toggle **assistant may run commands** is on (default off); with auto-run off,
control characters that would submit or signal on their own are stripped from
typed text, so nothing runs without your Enter. Toggle `🔊` to have replies
spoken aloud (macOS voices); dictate into the box with the macOS mic key
(`🎤` / double-Fn).

## Features

- **Workspaces** — named groups of project folders. The `▚` chip in the top
  bar names the active workspace; click it to switch, create, or add folders.
  The tree shows every folder in the workspace; whatever you click becomes the
  *active root* that new panes and the git widget follow.
- **Agent panes** — the `＋` menu spawns Claude Code, opencode, or pi in a
  real PTY (login shell, your prompt, your keybindings). Agents light up
  automatically when their CLI appears on `PATH` — no config.
- **Flows** — a `<name>.flow.json` file under `.tome/flows/` is a small
  directed graph of agent nodes: what each expects from upstream, what it
  produces, and the edges wiring one node's output to another's input. The
  `＋` menu's **Flow diagram…** creates one; opening it draws the canvas —
  drag nodes, draw edges, edit a node's brief in a modal. **Run** topologically
  sorts the graph and spawns one terminal per node with its brief pasted in —
  nothing is ever auto-submitted, so you review and press Enter yourself.
  Starter graphs live in [examples/flows/](examples/flows/).
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
- **App login** — once a passphrase is set, Tome locks at launch: unlock with
  Touch ID or passphrase (+ authenticator code when enrolled). The gate is
  enforced in the main process — pty, fs, git, chat, and brain IPC all refuse
  until login — not just painted over. First run offers setup (skippable).

## Platform support

macOS first. The allowlist proxy is platform-neutral and genuinely ports, but
the sandbox is seatbelt-only today — Linux would need bwrap/namespaces, which
is design-compatible but unbuilt. Performance questions (memory with N panes,
how to measure): [docs/perf.md](docs/perf.md).

## Security

The security claim in one sentence: agent CLIs run sandboxed with no direct
network egress, and the only route out — a per-pane allowlist proxy — widens
only behind a second factor, never the sandbox itself. The conductor's
scrollback→model→tool-call loop is treated as a confused deputy and capped
accordingly. Two independent reviews scored the codebase **8.0/10**
([kimi-k3 council](reviews/kimi-k3-review.txt), 2026-08-07;
[pi](reviews/pi-review.md), 2026-08-07); findings are tracked in
[docs/IMPROVEMENTS-STATUS.md](docs/IMPROVEMENTS-STATUS.md). See
[SECURITY.md](SECURITY.md) for the evaluation-facing summary and vulnerability
reporting, and [docs/THREATMODEL.md](docs/THREATMODEL.md) for the
maintainer-facing invariants.

## Stack

Electron + electron-vite · dockview-core (pane grid) · @xterm/xterm +
node-pty (real PTYs) · CodeMirror 6 · mammoth + SheetJS (documents) ·
@anthropic-ai/sdk (assistant).

## License

[MIT](LICENSE)
