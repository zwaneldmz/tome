# Tome Threat Model

This document collects the load-bearing security invariants that are
currently scattered through comments in `src-tauri/src/` (and the JS that
stays in `src/shared/` + `src/renderer/`). If you change code listed here,
check the invariant still holds — these are the assumptions the rest of the
design leans on.

## Assets

- `<app_data_dir>/egress-auth.json` — scrypt passphrase hash + TOTP secret
  (0600; the TOTP secret itself lives in the OS keychain when one is
  available — see `src-tauri/src/authlock.rs`).
- `<app_data_dir>/egress.json` — egress allowlist for gapped panes.
- `<app_data_dir>/egress-repo-consents.json` — repo egress consents (0600).
- `<app_data_dir>/events.jsonl` — the persistent event log.
- Provider API keys (Anthropic/OpenAI/etc.) read from the user's login shell.
- The user's project files, exposed to agent CLIs running in pty panes.

`<app_data_dir>` is Tauri's per-OS app-data directory — the analogue of
Electron's `app.getPath('userData')` (see `src-tauri/src/lib.rs`).

## Trust boundaries

1. **Renderer ↔ backend.** The renderer is a Tauri webview (there is no
   Electron `sandbox: true` flag) and talks to the Rust backend over Tauri
   IPC, gated fail-closed (`src-tauri/src/lock_gate.rs`). A compromised
   renderer must not be able to spawn arbitrary processes, read credentials,
   or weaken the egress controls.
2. **Agent pane ↔ host.** Agent CLIs run under the macOS seatbelt
   (`sandbox-exec`, `src-tauri/src/egress/seatbelt.rs`) or the Linux
   bubblewrap/unshare ladder (`src-tauri/src/egress/linux.rs`) with all
   direct egress denied; the only way out is a per-pane loopback CONNECT
   proxy that enforces the provider allowlist.
3. **Model ↔ tools.** The assistant chat (conductor, `src-tauri/src/conductor/`)
   reads terminal scrollback and issues tool calls; tool output and
   scrollback are untrusted input to the model, and model output drives
   actions on the workspace.

## Invariants

### 1. `store:get`/`store:set` stay open pre-login ⇒ store keys are vetted

Every `#[tauri::command]` refuses while the app is locked unless its channel
is on the explicit `OPEN_CHANNELS` allowlist (`src-tauri/src/lock_gate.rs`) —
which includes `store:get`/`store:set` because the lock screen itself needs
them. Because they are reachable before authentication, keys are strictly
vetted in `src-tauri/src/store_keys.rs` (used by `src-tauri/src/store.rs`):

- `is_key_shape_valid()` accepts only slug-shaped keys
  (`/^[a-z0-9][a-z0-9-]*$/`) — no path traversal into other files in
  `<app_data_dir>`.
- `RESERVED_KEYS = { egress, egress-auth, egress-repo-consents, events }` can
  never be read or written through the store — the credential file, the
  egress allowlist, the repo-consent file, and the event log are
  unreachable over this open channel.
- While locked, only `LOCKSCREEN_STORE_KEYS = { theme }` is touchable; every
  other well-shaped key is refused until login.

### 2. Login already proves the passphrase ⇒ pane unlock is second-factor-only

`egress:unlock` (freeing a gapped pane onto the open internet) is itself
behind the lock gate (`src-tauri/src/lock_gate.rs`), so the caller has
already proven the passphrase (or Touch ID) at login. Pane unlock therefore
demands a *second* factor by design: the TOTP code when enrolled, the
passphrase again otherwise (`src-tauri/src/authlock.rs`). Do not
"convenience" this down to a single click — re-proving something is the
point.

### 3. The note vault lives outside `<app_data_dir>` because the sandbox denies writes there

The per-workspace note vault lives at `~/Tome/Brains/<ws>` (sanitized
workspace name), not under Tauri's `<app_data_dir>`, precisely because the
seatbelt profile denies gapped panes all writes under the app config dir
(`src-tauri/src/egress/seatbelt.rs`) — and the Linux bwrap wrap replaces the
config dir with a fresh tmpfs (`src-tauri/src/egress/linux.rs`). This gives
agents full read/write of their vault with zero sandbox changes. The same
profile also denies reads of `egress-auth.json` (TOTP secret) specifically.
If the vault location ever moves (`src-tauri/src/brain.rs`'s `brains_root`),
re-check the seatbelt profile against it.

Known gap on Linux: the self-unshare fallback rung of the sandbox ladder
parses `--deny-write`/`--deny-read` (its Landlock file-confinement targets)
but does **not** enforce them yet — file confinement there is a documented
TODO (`src-tauri/crates/tome-shim/src/linux.rs`, `TODO(landlock)`). The
network-namespace egress kill is what that rung actually delivers; its
filesystem hardening is still open.

### 4. Unlock widens the proxy, never the sandbox

Unlocking a pane flips its per-pane proxy from "providers allowlist" to
"open" (`src-tauri/src/egress/mod.rs`'s `PaneMode`,
`src-tauri/src/egress/proxy.rs`'s `host_allowed`). The sandbox wrap (macOS
seatbelt profile / Linux bwrap or self-unshare argv) is fixed at spawn and
**no code path weakens the sandbox after spawn** — sandboxed processes can't
be re-profiled, and we don't try. All lock/unlock/relock state lives in the
backend proxy. Corollary: the sandbox denies egress even in "open" mode; the
proxy is still the only route out. `egress/proxy.rs` also re-checks
pane-alive + host-allowed at CONNECT-completion time (TOME-002), so a tunnel
that was only ever allowed because the pane was in `Open` mode can't finish
handshaking after a relock and pipe forever.

### 5. The scrollback → model → tool-call confused-deputy loop

The conductor reads terminal scrollback (`read_terminal`) and the model's
tool calls act on the workspace (`type_in_terminal`, `open_pane`,
`open_file`) — `src-tauri/src/conductor/tools.rs`. Scrollback is
attacker-influenceable (any program can print text that looks like
instructions), so the loop is a classic confused deputy. Caps on the worst
case:

- `type_in_terminal` only submits (sends `\r`) when the user has enabled
  "assistant may run commands" (`allowRun`,
  `src-tauri/src/conductor/state.rs`). With auto-run off, control
  characters that would submit or signal on their own (CR/LF, Ctrl-C,
  Ctrl-D, …) are stripped from the typed text, so `text: "ls\r"` cannot run
  a command the user never approved — the text sits in the prompt for review.
- `read_terminal` refuses gapped panes outright (TOME-009) and otherwise
  asks once per pane before disclosing scrollback.
- Scrollback is ANSI/control-stripped (`strip_ansi`) before the model sees
  it.
- Scrollback is capped (`SCROLL_CAP = 200_000`,
  `src-tauri/src/conductor/state.rs`).
- The tool loop is bounded (`MAX_TURNS = 8`,
  `src-tauri/src/conductor/chat.rs`).

### 6. `TOME_SHOT` dev bypass

`TOME_SHOT` (screenshot/demo mode) bypasses the lock gate and opens a
representative set of panes for development screenshots. It exists for
development only. Intent: it must never be reachable in a packaged build —
`src-tauri/src/lib.rs`'s `boot_auth_and_egress` computes it as
`truthy_env("TOME_SHOT") && tauri::is_dev()` (the port of
`!app.isPackaged`), and `lock_gate::is_locked` receives `shot_mode` as a
parameter rather than reading the env itself. Until that is independently
verified end-to-end, treat `TOME_SHOT` as a full auth bypass and never ship
it set.

### 7. `xlsx` is fetched from the SheetJS CDN, not the npm registry

`package.json` pins `xlsx` to `https://cdn.sheetjs.com/xlsx-0.20.3/xlsx-0.20.3.tgz`
because SheetJS stopped publishing to the npm registry after 0.18.5; the CDN
is the vendor's official distribution channel for current versions. The
lockfile pins the tarball's integrity hash, so the bytes are verified at
install. Do not "fix" this to an npm-registry version — those are stale or
third-party repacks.

### 8. Custom agents widen the spawn allowlist by user consent — the backend still owns the command line

Users may declare their own agent CLIs (Preferences → Agents, store key
`custom-agents`) and spawn them as pane kinds. This deliberately widens the
spawn allowlist beyond `src/shared/pane-kinds.js` (whose `AGENTS` list
`src-tauri/src/agent_spawn.rs` mirrors as its Rust-side copy), and it holds
only because the invariants of that allowlist are kept, verbatim, on the new
entries:

- **The backend re-vets on every use.** `merge_agents()`
  (`src-tauri/src/custom_agents.rs`) re-runs `vet_custom_agent` over every
  stored entry on every read — `pty:create` (`src-tauri/src/ipc/pty.rs`),
  `agents:list`, conductor prompt rebuild. Neither the store bytes nor the
  renderer's copy of the rules is trusted; a bad entry is dropped, not
  repaired, so a poisoned store degrades to "fewer kinds in the ＋ menu".
- **`bin` is a bare command name, resolved by the login-shell PATH** — never
  an absolute path, never a renderer-supplied path. Exactly how the
  built-ins resolve (`src-tauri/src/login_env.rs`).
- **`args` are inert literals.** Each token is character-guarded (printable
  ASCII, no spaces, no shell metacharacters) because the result is joined
  into the same `zsh -l -c` line the built-ins run on — the guard is
  load-bearing for the same reason `is_safe_model` is in
  `src-tauri/src/agent_spawn.rs`.
- **The renderer never supplies a binary or arguments at spawn time.**
  `pty:create` still takes only a `kind`; the backend resolves it against
  its own freshly re-vetted merged list and builds the command line from the
  list's own copies.
- **Pre-login writes are confined.** `store:set` is in `OPEN_CHANNELS`
  (invariant 1), so a locked app's renderer *can* write `custom-agents` —
  but only slug-shaped keys are writable at all, and a written custom only
  takes effect at spawn, and every spawn path (`pty:create`, `runs:start`)
  is lock-gated. The write can therefore queue a vetted-shape entry for
  later, never execute one.

## Secondary invariants worth knowing

- **Renderer names a pane kind, the backend builds the command line.**
  `pty:create` takes a vetted `kind` (built-ins from
  `src-tauri/src/agent_spawn.rs`, plus vetted custom kinds — invariant 8); a
  compromised renderer cannot request arbitrary binaries or arguments.
- **Agent secrets are handed only to agent panes.** Provider keys are read
  once from an interactive login shell (`src-tauri/src/login_env.rs`) and
  merged into the env of agent panes only — plain terminals inherit nothing,
  and Tome's own process env is untouched.
- **Chat API key never enters the renderer.** The Anthropic/Requesty client
  lives in the Rust backend (`src-tauri/src/ipc/chat.rs`); the renderer gets
  streamed deltas over Tauri events.
- **Brain paths are confined.** Every note/folder path from the renderer is
  resolved inside the vault root (`confine()` in `src-tauri/src/confine.rs`,
  `confine_real()` in `src-tauri/src/brain.rs`): no absolute paths, no `..`
  segments, symlink-escape refused; notes must end in `.md`; `AGENTS.md` is
  delete-protected.
- **`tome://` protocol is embed-only.** It serves confined,
  extension-allowlisted file bytes to the doc viewer's iframe
  (`src-tauri/src/protocol.rs`), but renderer JS cannot *read* `tome://`
  response bodies: the CSP `connect-src` omits `tome:` (see that file's
  module doc comment for the two-half fix).
- **A repo's `.tome/egress.json` is untrusted input.** It is validated by
  the same wildcard compiler as the user allowlist, over-broad patterns
  (bare `*`, `*.com`, single labels, URL syntax) are refused
  (`src-tauri/src/egress/allowlist.rs`), and the user must consent before
  any of it is honored. Consent is collected in the renderer but **verified
  and stored in the backend**
  (`<app_data_dir>/egress-repo-consents.json`, 0600, seatbelt-denied to
  agents; `src-tauri/src/egress/mod.rs`): the backend re-reads and re-hashes
  the file at consent time (TOCTOU-safe — a hash mismatch refuses) and at
  every boot and workspace sync, dropping consents whose file changed or
  vanished, so a post-consent edit re-prompts and delete-the-file is a real
  revocation. A compromised renderer cannot widen egress — it can only *ask*
  the backend to re-check the file. A consent is a **standing grant pinned to
  the file's SHA-1**: it applies globally (to every gapped pane, not just
  the active workspace) until the file changes or the user revokes it —
  switching workspaces does not revoke it.
- **The event log records actions, never payloads.**
  `<app_data_dir>/events.jsonl` keeps a capped, append-only audit of
  security-relevant actions — conductor tool calls (name, pane/chat id,
  outcome), egress unlocks/relocks, blocked egress hosts
  (`src-tauri/src/eventlog.rs`, cap 5000; `src-tauri/src/events.rs`) — but
  tool *inputs/outputs* and typed text stay out of the log by design: they
  may contain secrets. The renderer reads it only through the lock-gated
  `events:list` channel.
