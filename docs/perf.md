# Performance and memory

The short version: Tome is one Tauri app — a Rust backend
(`src-tauri/src/`) and one webview renderer (`src/renderer/`) — plus one
helper process per pane (the shell or agent CLI). There is no per-pane
Electron/browser-process overhead: a 6-pane workspace runs the same single
webview as a 1-pane workspace. We don't publish a single "footprint" number
because it depends on your machine, your scrollback, and which agents you
run; the model and the measuring commands below are more useful than a
number from our hardware.

## What to expect

**Per-pane cost.** Each pane is three things: one pty child process (the
shell or agent CLI — its memory is whatever the agent itself uses, owned by
the Rust backend via `src-tauri/src/pty.rs`, not node-pty), one xterm.js
instance in the single webview renderer, and one loopback CONNECT-proxy
socket when the pane is air-gapped. Spawning a pane adds a process, not a
webview.

**The backend is one Rust process.** It holds the pty handles
(`src-tauri/src/pty.rs`), the air-gap proxies
(`src-tauri/src/airgap/proxy.rs`), and the event log
(`src-tauri/src/events.rs`). It does not grow a new window, webview, or
BrowserView per pane.

**Memory scales with panes, not workspaces.** Switching workspaces tears
down panes; an idle workspace costs its files on disk, not resident
processes. What grows with pane count is pty child processes and xterm
instances — not the app shell itself.

**Scrollback is capped.** The backend keeps a per-pty tail of raw output for
the conductor's `read_terminal` tool, capped at 200,000 characters
(`SCROLL_CAP` in `src-tauri/src/conductor/state.rs`) — old output is dropped,
so a chatty agent can't grow memory without bound.

**The event log is capped.** On disk, the event log keeps at most 5,000
records (`CAP` in `src-tauri/src/eventlog.rs`); older records are compacted
away.

## Measure your own

Total resident memory of the Tauri app process:

```sh
ps -A -o rss,comm | grep -i "tome" | awk '{s+=$1} END {print s/1024 " MB"}'
```

That sums the Tauri app process (Rust backend + webview). The per-pane pty
processes — `zsh`, `claude`, `opencode`, `pi`, … — are separate child
processes; include them if you want the full per-pane cost.

Or open Activity Monitor, find Tome, and look at the process tree — the
per-pane pty processes show up as children, so you can see exactly what each
agent costs.

A stress check: spawn 6–10 panes (mix of agents and plain terminals), run
something chatty in each (`yes` works, a build is more realistic), and
re-run the command above. What to watch: renderer frame drops when more than
~10 xterm instances redraw simultaneously — that's the first resource that
actually degrades, and it shows up as visual stutter before it shows up as
memory.

## Startup: what loads when

Boot is ordered so first paint waits on the minimum set of work:

- **Rust backend, before the window:** `lib.rs::run()`'s `.setup()` runs the
  best-effort Electron→Tauri data migration, then `boot_auth_and_airgap`
  (auth load, repo-consent load, initial lock state). `mammoth`/`xlsx` no
  longer live in the backend at all — see the renderer item below.
- **Renderer, paint-critical path:** `bootTheme` → `bootAuth` → `bootChrome`
  → persisted-state reads → `restoreLayout` (`src/renderer/renderer.js`).
  Nothing else is awaited before the layout is on screen.
- **Renderer, post-paint tail:** git polling/menu, repo air-gap consent
  check, an idle-callback warm-up of the editor's CodeMirror language table
  and the brain pane's markdown mode, and `stt:warmup` (whisper model
  pre-load, gated on the `voice-warmup` store key, default off).
- **Lazy renderer chunks:** `@codemirror/language-data` (the ~600 kB
  language table) loads as a 37 kB description chunk on the first editor
  open (or the idle warm-up), with each language implementation an
  additional on-demand chunk behind it. The brain pane loads only the
  markdown chunk — it never touches the table. `mammoth` and `xlsx` load
  lazily too, in the renderer (`src/renderer/doc-convert.js`), on the first
  docx/xlsx open (promise-cached `import()`), so their parse cost is off
  every cold start.

### Boot profiling

Launch with `TOME_PROFILE=1` to get the renderer boot marks. The flag still
exists: the backend reads it in `src-tauri/src/lib.rs`'s `boot_plugin` and
exposes it to the renderer as `window.__TOME_BOOT__.profile` (mirrored as
`tome.profile`); the renderer prints one line of `performance.now()` marks —
module evaluation, `bootTheme`/`bootAuth`/`bootChrome`/`restoreLayout` — to
its console at boot end (`src/renderer/renderer.js`).

What changed from the Electron build: the old *main-process* timeline
("app ready", "pre-window init done", "window created", "did-finish-load")
had no direct Rust/Tauri equivalent and is no longer printed — the
`TOME_PROFILE` mechanism now covers the renderer marks only. The
Electron-era numbers below have not been re-measured for the Tauri build;
treat them as historical, not current.

Measured on an M-series MacBook (dev build, built renderer, median of 3 runs)
— **Electron-era, not re-measured for Tauri**:

| mark | before | after |
|---|---|---|
| app ready | 30ms | 29ms |
| pre-window init done | 94ms | 92ms |
| window created | 132ms | 122ms |
| did-finish-load | 240ms | 229ms |

The renderer entry chunk (`out/renderer/assets/index-*.js`, uncompressed)
— **Electron-era, not re-measured**:

| | before | after |
|---|---|---|
| entry chunk | 1,788,404 B | 1,752,836 B (−36 kB, language-data table split out) |

The main-process win from lazying `mammoth`/`xlsx` no longer applies as a
require-time cost — those libs moved into the renderer and are now part of
the renderer's lazy-chunk math.

## Known costs

- **Renderer bundle.** The Vite build (`dist-web/`) emits one entry chunk
  plus on-demand chunks (the CodeMirror language table, `xlsx`, the
  `mammoth` converter, the format/prettier worker). Exact sizes depend on the
  build; they load once per app launch, not per pane.
- **SheetJS pinned tarball.** The spreadsheet dependency is a pinned tarball
  (integrity-verified via the lockfile — see THREATMODEL.md), so installs
  fetch it as a single archive rather than from the registry.

## FAQ

**"How much memory with 6 agent panes?"** — Measure it on your machine with
the commands above; the architecture is one Tauri app (Rust backend + one
webview renderer) + N pty child processes + N loopback proxy sockets — there
is no per-pane webview or browser overhead.
