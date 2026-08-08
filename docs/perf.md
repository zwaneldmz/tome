# Performance and memory

The short version: Tome is one Electron main process, one renderer, and one helper process per pane. There is no per-pane Electron overhead — a 6-pane workspace has the same number of browser processes as a 1-pane workspace. We don't publish a single "footprint" number because it depends on your machine, your scrollback, and which agents you run; the model and the measuring commands below are more useful than a number from our hardware.

## What to expect

**Per-pane cost.** Each pane is three things: one `node-pty` process (the shell or agent CLI — its memory is whatever the agent itself uses), one xterm.js renderer in the single Chromium renderer process, and one loopback CONNECT-proxy socket when the pane is air-gapped. Spawning a pane adds a process, not a browser.

**The main process is one Node process.** It holds the pty handles, the air-gap proxies, and the event log. It does not grow a new window, webview, or BrowserView per pane.

**Memory scales with panes, not workspaces.** Switching workspaces tears down panes; an idle workspace costs its files on disk, not resident processes. What grows with pane count is pty processes and xterm instances — not Electron itself.

**Scrollback is capped.** Main keeps a per-pty tail of raw output for the conductor's `read_terminal` tool, capped at 200,000 characters (`SCROLL_CAP` in `src/main/conductor.js`) — old output is dropped, so a chatty agent can't grow memory without bound.

**The event log is capped.** On disk, the event log keeps at most 5,000 records (`CAP` in `src/main/lib/eventlog.js`); older records are compacted away.

## Measure your own

Total resident memory of the whole Tome tree:

```sh
ps -A -o rss,comm | grep -i "tome\|electron" | awk '{s+=$1} END {print s/1024 " MB"}'
```

Or open Activity Monitor, find Tome, and look at the process tree — the per-pane pty processes show up as children, so you can see exactly what each agent costs.

A stress check: spawn 6–10 panes (mix of agents and plain terminals), run something chatty in each (`yes` works, a build is more realistic), and re-run the command above. What to watch: renderer frame drops when more than ~10 xterm instances redraw simultaneously — that's the first resource that actually degrades, and it shows up as visual stutter before it shows up as memory.

## Known costs

- **node-pty rebuild on install.** The `postinstall` runs `electron-rebuild` so the pty native module matches Electron's ABI. First install compiles; that's expected, not a hang.
- **~1.7 MB renderer bundle.** The largest chunk from `electron-vite build` is ~1.76 MB uncompressed (xterm.js plus the UI). It loads once per app launch, not per pane.
- **SheetJS pinned tarball.** The spreadsheet dependency is a pinned tarball (integrity-verified via the lockfile — see THREATMODEL.md), so installs fetch it as a single archive rather than from the registry.

## FAQ

**"How much memory with 6 agent panes?"** — Measure it on your machine with the commands above; the architecture is one Electron main + one renderer + N pty processes + N loopback proxy sockets — there is no per-pane Electron overhead.
