# WS3 — Layout persistence, performance & UX

Branch: `improve/ws3` (3 commits). **Build: `npm run build` passes** (electron-vite, all three bundles).

Note: `reviews/pi-review.md` is not present in this worktree (nor in git history); worked from the WS3 section of `docs/IMPROVEMENTS-PLAN.md`, which summarizes recs #6, #8, #11, #13.

## Task 1 — Dockview layout persistence per workspace
- `dock.toJSON()` serialized on every `onDidLayoutChange` (800 ms debounce) and once more on quit via a new before-quit handshake (main sends `app:before-quit`, renderer persists then replies `app:quit-ready`; main force-quits after 1.5 s regardless). Channels added to the preload allowlist (`tome.app.onBeforeQuit/quitReady`); `before-quit` fires on every quit path including `window-all-closed`.
- Store key: `layout-<slug>` where the slug derives from the workspace's **folder list** (falling back to the name for folder-less workspaces) so renaming a workspace keeps its layout; slug is vetted-store-key-safe.
- Restore on boot: `dock.fromJSON()` rebuilds the grid shell (id/title/params/position), then each panel is respawned through the same code paths as manual opens (`spawnTerminal/spawnChat/spawnBrain/spawnHistory/openFile` refactored to accept a `saved` panel and position the replacement with `{ referencePanel: saved.id }`, after which the placeholder is disposed).
- **Terminals: recreated as fresh shells in their saved positions** (same kind/cwd/airgap), not skipped — a pty is a live process and can't be resumed; the grid shape survives even though scrollback/processes don't. Documented in the "layout persistence" comment block in renderer.js.
- Stale restores: missing files → editor/doc panel skipped; missing dirs → history panel skipped; brain panes for deleted workspaces skipped; unknown components dropped; corrupt layout JSON → `dock.clear()` and start empty; doc panels whose iframe fails to deserialize are detected (`view.content.element` not connected) and removed.

## Task 2 — Single instance
`app.requestSingleInstanceLock()` at startup; `second-instance` restores (if minimized) and focuses the existing window. Second launch quits itself.

## Task 3 — Async boot
- `resolveLoginPath()` + `resolveAgentSecrets()` (both `execFileSync`, up to 8 s each on the launch path) merged into one `ensureLoginEnv()` that spawns the login shell **twice in parallel** (`Promise.allSettled`) and caches one shared promise.
- Fired once at boot so the shell-out overlaps window creation; every consumer (`pty:create`, `agents:list`, chat provider) awaits the same in-flight promise — only the first caller pays.
- `resolveAgentSecrets()` is now async and lazy: secrets are only read out of the cached env at first agent spawn / chat send; tome's own process env still untouched. Behavior otherwise identical (same PATH parse, same well-known-bin fallback, same ponytail secret regex). Zero `execSync`/`execFileSync` remain in src/main.

## Task 4 — Pty data coalescing
Per-pane buffer in main (`ptyBuffers` map): output accumulates and flushes on a 4 ms timer or at 64 KB buffered, whichever first. Buffers are flushed on pane exit and kill so output isn't stranded. `conductor.record()` still sees raw unbuffered chunks. Renderer untouched — `xterm.write()` accepts batched strings.

## Task 5 — Chat stop button + budget + resend
- New `chat:abort` send channel (preload allowlist) → `conductor.abortChat(id)` → `AbortController` passed to `anthropic.beta.messages.stream(args, { signal })`; abort ends the loop and sends `chat:done { aborted: true, error: 'Stopped.' }`.
- Cumulative token budget: 400k tokens (`final.usage` summed across the 8-turn tool loop); on exceed, graceful `chat:done` with a visible note ("Token budget reached (~Nk tokens…) — stopped early. Ask again to continue."). Budget is checked between turns (usage only exists per completed turn).
- Renderer: magenta ■ stop button in the chat form, visible only while busy.
- On failure the user message is popped from history **and restored to the input** for resend (previously silently dropped). Aborted sends keep the message in history (it was partially answered). Non-abort done events (refusal, budget, loop limit, auth errors) carry `aborted: false` so they also restore the input.

## Task 6 — Toast history
Every `toast()` is appended to a session-only log (capped at 100). A bell (♪) button in the top bar opens a menu listing entries newest-first with 24 h timestamps, colored left border (red/cyan by kind), plus a Clear action. The bell glows magenta when entries are unseen. Styles match the existing neon-on-black theme (`--magenta`/`--cyan`/`--panel`).

## Deviations / notes
- Layout store keys are folder-based rather than "workspace path" (workspaces are name+folders, no single path); documented above.
- Agent CLIs restored from a layout are relaunched (fresh process), not just shells — same as terminals, since the pane kind is persisted.
- Could not run the packaged-app smoke test in this environment (direct `Electron` binary launch from this shell fails macOS sandbox init — pre-existing environment limitation, unrelated to the changes). Verified via `npm run build` and close reading of dockview-core 4.13.1 internals (`fromJSON`, deserializer, panel model) for API correctness.
- `renderer.js` edited in place per constraints; no new IPC outside the preload allowlist; no tests added (WS1); no security findings touched (WS2).
