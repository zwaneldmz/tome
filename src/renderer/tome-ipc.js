// Frontend IPC shim: rebuilds the Electron preload's `window.tome` bridge
// (src/preload/index.js, ~71 channels) over Tauri v2's global API
// (window.__TAURI__, enabled via app.withGlobalTauri in tauri.conf.json).
// Imported first thing in renderer.js/popout entries so every other
// module's read of `window.tome` (see util.js) sees a real bridge —
// Electron's own preload where present, this shim otherwise. No
// @tauri-apps/api npm dependency; everything comes off the injected global.
//
// Wire-naming binding decision: Electron channel "domain:verb" becomes
// Tauri command "domain_verb" (snake_case). Main→renderer events keep their
// exact colon wire names ("pty:exit", "events:appended", …) via Tauri's
// `listen()` — colons are legal in Tauri event names and the Rust side
// emits on them verbatim, so no translation happens for those.
//
// Command argument shapes mirror what preload already sent. Most preload
// channels already pass a plain object ({id, data}, {dir, branch, create},
// …) — those keys are reused as-is. A handful pass a single bare scalar
// (fs:readDir(p), store:get(key), conductor:allowRun(v), …); Electron IPC
// has no named-argument concept for a bare scalar, so this shim wraps each
// in a descriptive single-key object (see the per-call comments below).
// Tauri's JS→Rust argument convention is camelCase-in/snake_case-out (e.g.
// `{ paneId }` reaches a Rust parameter named `pane_id`), so whichever task
// writes the matching #[tauri::command] signatures needs to match the key
// names chosen here — they are a judgment call, not read off any contract.
;(function initTomeIpc() {
  // Electron's preload already installed the real bridge — coexistence
  // holds through Phase 7, so both runtimes share this one file.
  if (window.tome) return
  if (!window.__TAURI__) {
    console.warn(
      '[tome-ipc] window.__TAURI__ is not present (not running under Tauri or Electron) — window.tome will be unavailable.'
    )
    return
  }

  const { invoke, Channel } = window.__TAURI__.core
  const { listen } = window.__TAURI__.event

  // Tauri command rejections are plain strings (the Err(String) arm of
  // Result<serde_json::Value, String>), but every renderer call site that
  // awaits tome.* and handles failure was written against Electron, which
  // always rejects with a real Error and reads `err.message`. Normalize
  // once, here, instead of at every call site.
  function call(cmd, args) {
    return invoke(cmd, args).catch((e) => {
      throw e instanceof Error ? e : new Error(String(e))
    })
  }

  // Electron's ipcRenderer.send has no return value and no delivery
  // guarantee; every preload "send channel" mirrors that — fire the
  // invoke, never await it, just log if the command rejects.
  function fire(cmd, args) {
    invoke(cmd, args).catch(console.warn)
  }

  // Plain event subscription — mirrors ipcRenderer.on(chan, (e, payload) =>
  // cb(payload)). Preload never hands back an unsubscribe for these, so
  // neither do we (only events:appended / runs:changed get that treatment).
  function on(event, cb) {
    listen(event, (e) => cb(e.payload)).catch(console.warn)
  }

  // Same, for the two zero-payload events whose preload callback takes no
  // argument at all (app:before-quit, app:open-preferences).
  function onNoPayload(event, cb) {
    listen(event, () => cb()).catch(console.warn)
  }

  // events.onAppended / runs.onChanged are the only two preload subscribers
  // that hand back an unsubscribe (a disposed pane must stop receiving
  // pushes — ipcRenderer.on returns an emitter with no .dispose, so preload
  // wraps it). Tauri's listen() resolves a Promise<UnlistenFn> instead of
  // returning one synchronously, so the unsubscribe here queues behind that
  // registration and only then calls it.
  function onWithUnsub(event, cb) {
    const p = listen(event, (e) => cb(e.payload))
    return () => p.then((un) => un())
  }

  let warnedPathForFile = false

  // Every pty pane's data stream multiplexes through one Channel per
  // pty.create() call (Tauri Channels, not the event bus — the plan's PTY
  // streaming decision); onData subscribers are global across panes, same
  // as the preload's single ipcRenderer.on('pty:data', …) listener was.
  const ptyDataSubs = []

  // ---- fs.format: prettier runs in a renderer Web Worker (plan §Prettier)
  // ----
  // Created lazily, on the FIRST fs.format() call rather than at boot, so a
  // session that never saves with format-on-save never pays for Prettier's
  // plugin bundle at all. `fmt_format` (the Tauri command, ipc/fmt.rs)
  // still exists as a safe fallback but is deliberately not called from
  // here — this is the "shim calls the worker directly, no Rust
  // round-trip" wiring the phase 5a-docs task chose as the cleanest path,
  // since prettier/standalone needs no privileged access at all.
  let fmtWorker = null
  let fmtReqId = 0
  const fmtPending = new Map() // id -> resolve
  const FMT_TIMEOUT_MS = 10000 // a wedged worker must never hang Mod-S forever

  function settleFmt(id, value) {
    const resolve = fmtPending.get(id)
    if (!resolve) return // already settled (timeout raced the real reply)
    fmtPending.delete(id)
    resolve(value)
  }

  function getFmtWorker() {
    if (fmtWorker) return fmtWorker
    try {
      fmtWorker = new Worker(new URL('./fmt-worker.js', import.meta.url), { type: 'module' })
    } catch (err) {
      console.warn('[tome-ipc] fmt worker failed to start:', err)
      return null
    }
    fmtWorker.onmessage = (e) => settleFmt(e.data.id, e.data.value)
    fmtWorker.onerror = (err) => {
      // A worker-level error (script failed to load, an exception outside
      // fmt-worker.js's own try/catch) leaves any in-flight requests with
      // no reply coming — resolve them `null`, the same "no parser for
      // this file type" shape a save-with-format-on-save already treats as
      // a no-op, rather than let them hang on the timeout below.
      console.warn('[tome-ipc] fmt worker crashed, will restart on next format:', err.message)
      for (const id of [...fmtPending.keys()]) settleFmt(id, null)
      fmtWorker = null // next call gets a fresh worker
    }
    return fmtWorker
  }

  function formatInWorker(path, content) {
    const worker = getFmtWorker()
    if (!worker) return Promise.resolve(null)
    return new Promise((resolve) => {
      const id = ++fmtReqId
      fmtPending.set(id, resolve)
      setTimeout(() => settleFmt(id, null), FMT_TIMEOUT_MS)
      worker.postMessage({ id, path, content })
    })
  }

  const boot = window.__TOME_BOOT__ || {}

  window.tome = {
    home: boot.home ?? '',
    shotMode: !!boot.shotMode,
    profile: !!boot.profile,

    pty: {
      create: (opts) => {
        const ch = new Channel()
        ch.onmessage = (m) => ptyDataSubs.forEach((f) => f(m))
        return call('pty_create', { opts, onData: ch })
      },
      write: (id, data) => fire('pty_write', { id, data }),
      resize: (id, cols, rows) => fire('pty_resize', { id, cols, rows }),
      kill: (id) => fire('pty_kill', { id }),
      onData: (cb) => {
        ptyDataSubs.push(cb)
      },
      onExit: (cb) => on('pty:exit', cb),
    },

    fs: {
      readDir: (p) => call('fs_read_dir', { path: p }),
      readFile: (p) => call('fs_read_file', { path: p }),
      writeFile: (path, content) => call('fs_write_file', { path, content }),
      mkdir: (p) => call('fs_mkdir', { path: p }),
      createFile: (p) => call('fs_create_file', { path: p }),
      watch: (p) => call('fs_watch', { path: p }),
      unwatch: (p) => call('fs_unwatch', { path: p }),
      onChanged: (cb) => on('fs:changed', cb),
      // Lives under `fs` in the preload object shape; the wire channel
      // this used to ride ('fmt:format') is now a renderer Web Worker
      // instead of a Tauri command — see the fmt-worker block above.
      format: (path, content) => formatInWorker(path, content),
    },

    store: {
      get: (key) => call('store_get', { key }),
      set: (key, value) => call('store_set', { key, value }),
    },

    webUtils: {
      // File.path is gone in modern Electron/Chromium and Tauri never had
      // it; drag-and-drop path resolution moves to the tauri://drag-drop
      // event in the phase 6 adapter. Until then, behave like a File whose
      // path could not be resolved.
      pathForFile: (file) => {
        if (!warnedPathForFile) {
          warnedPathForFile = true
          console.warn(
            '[tome-ipc] webUtils.pathForFile: drag-drop paths arrive via tauri://drag-drop — adapter lands in phase 6'
          )
        }
        return null
      },
    },

    // OS file drag-drop (phase 6, plan §8). tauri.conf.json's
    // app.windows[].dragDropEnabled makes Tauri intercept the native OS drag
    // before it ever reaches the DOM as a 'Files' drag — so File.path and
    // webUtils.pathForFile above never see a real drop under Tauri — and
    // deliver already-resolved, absolute OS paths straight from the
    // window/webview layer via tauri://drag-drop (paths + position) and its
    // …-enter/…-leave siblings (panes.js's hover highlight). No per-item
    // resolution step the way Electron's File + webUtils.getPathForFile
    // two-step needed.
    //
    // dragDropEnabled's interception is not scoped to file drags — it
    // gates every native drag session the webview sees, including
    // dockview's own in-page tab-header dragging. See panes.js's own
    // `tome.dragDrop` block (~line 178) for the source-verified mechanism
    // and why this is left on rather than switched off to work around it.
    //
    // Plain event.listen (this file's `on` helper — same as every other
    // subscription here) rather than the dedicated
    // webview.getCurrentWebview().onDragDropEvent() convenience wrapper some
    // Tauri examples use: verified directly against the pinned tauri crate
    // source (Cargo.lock pins tauri 2.11.5) that these events emit
    // window/webview-SCOPED (`emit_to_window`/`emit_to_webview`, not a
    // plain broadcast `emit`), but a listener registered with no explicit
    // target defaults to `EventTarget::Any`, and
    // `event/listener.rs::match_any_or_filter` special-cases `Any` to match
    // regardless of the emit-side scope filter — so the plain, untargeted
    // `listen()` this file already uses everywhere else does receive them.
    // One calling convention, and this app has exactly one window this
    // phase (popout v2 is out of scope — plan §8), so there's no
    // multi-webview ambiguity yet; a future popout window emitting its own
    // drag-drop would also reach this same listener and should be re-scoped
    // (via `{ target: { kind: 'Webview', label } }`) when that lands.
    //
    // Paths are handed to panes.js's drop handler exactly as received, with
    // NO extra confinement check added here: a physical OS drag from the
    // user's own file manager is the same "user-driven, not model-driven"
    // trust bucket src-tauri/src/confine.rs's module doc comment already
    // carves out for fs:readFile (unvetted by design — renderer compromise
    // already equals user-privileged file access for tree/editor opens);
    // panes.js feeds these into the exact same openFile() the DOM File-drag
    // path already used unconfined, so this is a behavior match, not a new
    // hole.
    dragDrop: {
      onEnter: (cb) => on('tauri://drag-enter', cb),
      onLeave: (cb) => on('tauri://drag-leave', cb),
      onDrop: (cb) => on('tauri://drag-drop', cb),
    },

    git: {
      info: (dir) => call('git_info', { dir }),
      branches: (dir) => call('git_branches', { dir }),
      checkout: (dir, branch, create) => call('git_checkout', { dir, branch, create }),
      log: (dir, limit) => call('git_log', { dir, limit }),
      commit: (dir, hash) => call('git_commit', { dir, hash }),
      diff: (dir, hash, file) => call('git_diff', { dir, hash, file }),
    },

    auth: {
      status: () => call('auth_status'),
      login: (opts) => call('auth_login', opts),
      touchid: () => call('auth_touchid'),
    },

    panes: {
      sync: (list) => fire('panes_sync', { list }),
    },

    ws: {
      syncFolders: (folders) => fire('ws_sync', { folders }),
    },

    conductor: {
      allowRun: (v) => fire('conductor_allow_run', { allow: v }),
      allowRead: (paneId, allowed) => fire('conductor_allow_read', { paneId, allowed }),
      onReadRequest: (cb) => on('conductor:readRequest', cb),
      onOpen: (cb) => on('conductor:open', cb),
      onActed: (cb) => on('conductor:acted', cb),
    },

    doc: {
      // Returns raw bytes ({ base64 }), not a pre-rendered { html } — the
      // renderer converts docx/xlsx itself now (see doc-convert.js and
      // panels/doc.js). Renamed from `read` to name that contract change
      // honestly, matching the Rust command's own doc_read -> doc_read_bytes
      // rename (ipc/doc.rs).
      readBytes: (p) => call('doc_read_bytes', { path: p }),
    },

    theme: {
      set: (pref, mode) => fire('theme_set', { pref, mode }),
    },

    openPath: (p) => call('shell_open_path', { path: p }),

    airgap: {
      state: () => call('airgap_state'),
      unlock: (opts) => call('airgap_unlock', opts),
      relock: (paneId) => call('airgap_relock', { paneId }),
      setup: (passphrase) => call('airgap_setup', { passphrase }),
      enrollTotp: () => call('airgap_enroll_totp'),
      confirmTotp: (code) => call('airgap_confirm_totp', { code }),
      readRepo: (root) => call('airgap_read_repo_allowlist', { root }),
      consentRepo: (root, hash) => call('airgap_consent_repo_allowlist', { root, hash }),
      revokeRepo: (root) => call('airgap_revoke_repo_allowlist', { root }),
      onBlocked: (cb) => on('airgap:blocked', cb),
      onState: (cb) => on('airgap:state', cb),
    },

    agents: {
      list: () => call('agents_list'),
      customs: () => call('agents_customs'),
      changed: () => fire('agents_changed'),
    },

    // Persistent event log (main/Rust owns app_data_dir/events.jsonl): a
    // read-only tail plus a live push for each new record.
    events: {
      list: () => call('events_list'),
      onAppended: (cb) => onWithUnsub('events:appended', cb),
    },

    // Background flow runs: main/Rust owns the child processes and is the
    // single writer of run.json, so the renderer only ever starts, stops,
    // and reads snapshots.
    runs: {
      start: (flowPath) => call('runs_start', { flowPath }),
      cancel: (id) => call('runs_cancel', { id }),
      list: () => call('runs_list'),
      onChanged: (cb) => onWithUnsub('runs:changed', cb),
    },

    stt: {
      transcribe: (wav) => call('stt_transcribe', { wav }),
      warmup: () => call('stt_warmup'),
      status: () => call('stt_status'),
    },

    chat: {
      send: (id, messages, brainWs) => call('chat_send', { id, messages, brainWs }),
      abort: (id) => fire('chat_abort', { id }),
      providers: () => call('chat_providers'),
      // Rust side emits these as plain events until the real chat command
      // lands (plan §Chat) — same wire names, same shim, no special-casing.
      onDelta: (cb) => on('chat:delta', cb),
      onDone: (cb) => on('chat:done', cb),
      onTool: (cb) => on('chat:tool', cb),
    },

    brain: {
      open: (ws) => call('brain_open', { ws }),
      close: (ws) => call('brain_close', { ws }),
      index: (ws) => call('brain_index', { ws }),
      read: (ws, rel) => call('brain_read', { ws, rel }),
      write: (ws, rel, content, exclusive) => call('brain_write', { ws, rel, content, exclusive }),
      delete: (ws, rel) => call('brain_delete', { ws, rel }),
      coreInfo: () => call('brain_core_info'),
      promote: (ws, rel, folder, overwrite, rename) =>
        call('brain_promote', { ws, rel, folder, overwrite, rename }),
      onChanged: (cb) => on('brain:changed', cb),
    },

    lsp: {
      didOpen: (path, text) => fire('lsp_did_open', { path, text }),
      didChange: (path, text) => fire('lsp_did_change', { path, text }),
      didClose: (path) => fire('lsp_did_close', { path }),
      hover: (path, line, character) => call('lsp_hover', { path, line, character }),
      definition: (path, line, character) => call('lsp_definition', { path, line, character }),
      onDiagnostics: (cb) => on('lsp:diagnostics', cb),
      onMissing: (cb) => on('lsp:missing', cb),
    },

    pickFolder: () => call('dialog_pick_folder'),
    pickFile: () => call('dialog_pick_file'),

    app: {
      onBeforeQuit: (cb) => onNoPayload('app:before-quit', cb),
      quitReady: () => fire('app_quit_ready'),
      onOpenPreferences: (cb) => onNoPayload('app:open-preferences', cb),
    },

    // Native menu bar: one generic channel, the renderer's menu-bridge
    // switches on action.id.
    menu: {
      onAction: (cb) => on('menu:action', cb),
    },

    // A popped-out window is trying to close. Main/Rust holds it open until
    // close() is called; never calling it leaves the window where it is.
    popout: {
      onCloseRequest: (cb) => on('popout:close-request', cb),
      close: (id) => call('popout_close', { id }),
      // Not present on the Electron preload object at all, so
      // `tome.popout.supported` is `undefined` there — falsy, so any
      // renderer code gating on it stays correct under Electron too. wry
      // cannot move live DOM into a same-context window.open() the way
      // dockview's popout does (the one genuine Phase-1 regression); real
      // popout support returns in Phase 6 via a dedicated WebviewWindow.
      supported: false,
    },
  }
})()
