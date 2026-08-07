# Tome Threat Model

This document collects the load-bearing security invariants that are currently
scattered through comments in `src/main/`. If you change code listed here,
check the invariant still holds — these are the assumptions the rest of the
design leans on.

## Assets

- `userData/airgap-auth.json` — scrypt passphrase hash + TOTP secret (0600).
- `userData/airgap.json` — egress allowlist for air-gapped panes.
- Provider API keys (Anthropic/OpenAI/etc.) read from the user's login shell.
- The user's project files, exposed to agent CLIs running in pty panes.

## Trust boundaries

1. **Renderer ↔ main.** The renderer is sandboxed (`sandbox: true`) and talks
   to main over IPC. A compromised renderer must not be able to spawn
   arbitrary processes, read credentials, or weaken the air gap.
2. **Agent pane ↔ host.** Agent CLIs run under the macOS seatbelt
   (`sandbox-exec`) with all direct egress denied; the only way out is a
   per-pane loopback CONNECT proxy that enforces the provider allowlist.
3. **Model ↔ tools.** The assistant chat (conductor) reads terminal scrollback
   and issues tool calls; tool output and scrollback are untrusted input to
   the model, and model output drives actions on the workspace.

## Invariants

### 1. `store:get`/`store:set` stay open pre-login ⇒ store keys are vetted

Every `ipcMain.handle` channel refuses while the app is locked, except an
explicit `OPEN_CHANNELS` allowlist — which includes `store:get`/`store:set`
because the lock screen itself needs them. Because they are reachable before
authentication, keys are strictly vetted in `src/main/index.js`:

- `vetKey()` accepts only slug-shaped keys (`/^[a-z0-9][a-z0-9-]*$/`) — no
  path traversal into other files in `userData`.
- `RESERVED_KEYS = { airgap, airgap-auth }` can never be read or written
  through the store — the credential file and the egress allowlist are
  unreachable over this open channel.

### 2. Login already proves the passphrase ⇒ pane unlock is second-factor-only

`airgap:unlock` (freeing an air-gapped pane onto the open internet) is itself
behind the lock gate, so the caller has already proven the passphrase (or
Touch ID) at login. Pane unlock therefore demands a *second* factor by
design: the TOTP code when enrolled, the passphrase again otherwise. Do not
"convenience" this down to a single click — re-proving something is the
point.

### 3. `brain/` lives outside `userData` because the seatbelt denies `userData` writes

The per-workspace note vault lives at `~/Tome/Brains/<ws>`, not under
Electron `userData`, precisely because the seatbelt profile denies air-gapped
panes all writes under `userData`. This gives agents full read/write of their
vault with zero sandbox changes. The same profile also denies writes to
`userData` generally (allowlist tamper) and reads of `airgap-auth.json`
(TOTP secret) specifically. If the vault location ever moves, re-check the
seatbelt profile against it.

### 4. The air gap widens the proxy, never the sandbox

Unlocking a pane flips its per-pane proxy from "providers allowlist" to
"open" (`airgap.js: hostAllowed`). The seatbelt profile is fixed at spawn and
**no code path weakens the sandbox after spawn** — sandboxed processes can't
be re-profiled, and we don't try. All lock/unlock/relock state lives in the
main-process proxy. Corollary: the sandbox denies egress even in "open" mode;
the proxy is still the only route out.

### 5. The scrollback → model → tool-call confused-deputy loop

The conductor reads terminal scrollback (`read_terminal`) and the model's
tool calls act on the workspace (`type_in_terminal`, `open_pane`,
`open_file`). Scrollback is attacker-influenceable (any program can print
text that looks like instructions), so the loop is a classic confused deputy.
Caps on the worst case:

- `type_in_terminal` only submits (sends `\r`) when the user has enabled
  "assistant may run commands" (`allowRun`). With auto-run off, control
  characters that would submit or signal on their own (CR/LF, Ctrl-C,
  Ctrl-D, …) are stripped from the typed text, so `text: "ls\r"` cannot run
  a command the user never approved — the text sits in the prompt for review.
- Scrollback is ANSI/control-stripped before the model sees it.
- The tool loop is bounded (8 turns).

### 6. `TOME_SHOT` dev bypass

`TOME_SHOT` (screenshot/demo mode) bypasses the lock gate
(`isLockedNow()` returns false) and captures the window to a PNG after load.
It exists for development screenshots only. Intent: it must never be
reachable in a packaged build — WS2 gates it on `!app.isPackaged`. Until
that lands, treat `TOME_SHOT` as a full auth bypass and never ship it set.

### 7. `xlsx` is fetched from the SheetJS CDN, not the npm registry

`package.json` pins `xlsx` to `https://cdn.sheetjs.com/xlsx-0.20.3/xlsx-0.20.3.tgz`
because SheetJS stopped publishing to the npm registry after 0.18.5; the CDN
is the vendor's official distribution channel for current versions. The
lockfile pins the tarball's integrity hash, so the bytes are verified at
install. Do not "fix" this to an npm-registry version — those are stale or
third-party repacks.

## Secondary invariants worth knowing

- **Renderer names a pane kind, main builds the command line.** `pty:create`
  takes a vetted `kind` from `src/shared/pane-kinds.js`; a compromised
  renderer cannot request arbitrary binaries or arguments.
- **Agent secrets are handed only to agent panes.** Provider keys are read
  once from an interactive login shell and merged into the env of agent
  panes only — plain terminals inherit nothing, and Tome's own process env
  is untouched.
- **Chat API key never enters the renderer.** The Anthropic/Requesty client
  lives in main; the renderer gets streamed deltas over IPC.
- **Brain paths are confined.** Every note/folder path from the renderer is
  resolved inside the vault root (`confine()` in `brain.js`): no absolute
  paths, no `..` segments; notes must end in `.md`; `AGENTS.md` is
  delete-protected.
- **`tome://` protocol is embed-only.** `corsEnabled` lets panes embed
  PDFs/images, but renderer JS cannot *read* `tome://` response bodies.
