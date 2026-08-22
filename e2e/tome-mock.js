// Playwright renderer-E2E mock for the `window.tome` IPC bridge.
//
// The real bridge (`src/renderer/tome-ipc.js`) only builds when
// `window.__TAURI_INTERNALS__` exists — which it never does in a plain
// Playwright browser. This script installs a full `window.tome` before any
// app module evaluates (util.js reads `window.tome` at module-eval time), so
// the renderer boots against an in-memory, record-able fake backend.
//
// It also exposes `window.__tomeMock` with:
//   - `store`:  the in-memory key/value store backing tome.store.get/set
//   - `calls`:  recorded calls, e.g. `calls.ptyCreate` is an array of the
//               opts objects passed to tome.pty.create
// Tests pre-seed `store` via addInitScript, or read `calls` after actions.
//
// Every command resolves (never rejects) and every event channel is a no-op
// subscriber, so the UI reaches a fully-rendered state with no real backend.

(function () {
  const store = {
    'onboarded-v1': true,
    'egress-default': true,
    'conductor-run': false,
    'docker-gateway': false,
  }
  const calls = { ptyCreate: [] }

  const noop = () => {}
  const on = () => () => {}
  const asyncNoop = async () => {}

  window.__tomeMock = { store, calls }

  window.tome = {
    home: '/Users/test',
    shotMode: false,
    profile: false,

    pty: {
      create: async (opts) => {
        calls.ptyCreate.push(opts)
        return {}
      },
      write: noop,
      resize: noop,
      kill: noop,
      onData: on,
      onExit: on,
    },

    fs: {
      readDir: async () => [],
      readFile: async () => null,
      writeFile: asyncNoop,
      mkdir: asyncNoop,
      createFile: asyncNoop,
      watch: asyncNoop,
      unwatch: asyncNoop,
      onChanged: on,
      format: async () => null,
    },

    store: {
      get: async (key) => (key in store ? store[key] : null),
      set: async (key, value) => {
        store[key] = value
      },
    },

    webUtils: { pathForFile: () => null },

    dragDrop: { onEnter: on, onLeave: on, onDrop: on },

    git: {
      info: async () => null,
      branches: async () => [],
      checkout: asyncNoop,
      log: async () => [],
      commit: async () => null,
      diff: async () => null,
      status: async () => null,
      stage: asyncNoop,
      commitCreate: asyncNoop,
      push: asyncNoop,
    },

    skills: { list: async () => [], read: async () => null },

    auth: {
      status: async () => ({ configured: false, totp: false, unlocked: true, touchId: false }),
      login: async () => ({ ok: true }),
      touchid: async () => ({ ok: true }),
    },

    panes: { sync: noop },
    ws: { syncFolders: noop },

    conductor: {
      allowRun: noop,
      allowRead: noop,
      onReadRequest: on,
      onOpen: on,
      onActed: on,
    },

    doc: { readBytes: async () => ({ base64: '' }) },

    theme: { set: noop },
    openPath: asyncNoop,

    egress: {
      state: async () => ({
        panes: {},
        defaultMinutes: 15,
        repo: [],
        auth: { configured: false, totp: false },
      }),
      unlock: async () => ({ ok: true }),
      relock: asyncNoop,
      setup: async () => ({ ok: true }),
      enrollTotp: async () => ({ secret: '', uri: '' }),
      confirmTotp: async () => true,
      readRepo: async () => ({ state: 'absent' }),
      consentRepo: async () => ({ ok: true, applied: [], rejected: [] }),
      revokeRepo: async () => ({ ok: true }),
      onBlocked: on,
      onState: on,
    },

    agents: {
      list: async () => [
        { name: 'claude', available: true, custom: false },
        { name: 'opencode', available: true, custom: false },
      ],
      customs: async () => [],
      changed: noop,
    },

    events: { list: async () => [], onAppended: on },

    runs: {
      start: asyncNoop,
      cancel: asyncNoop,
      list: async () => [],
      onChanged: on,
      export: asyncNoop,
    },

    exportDest: {
      list: async () => [],
      consent: async () => ({ ok: true }),
      revoke: async () => ({ ok: true }),
    },

    schedules: {
      list: async () => [],
      set: async () => ({ ok: true }),
      delete: asyncNoop,
    },

    remote: {
      sources: async () => [],
      consent: async () => ({ ok: true }),
      revoke: async () => ({ ok: true }),
      runs: async () => [],
      runDetail: async () => null,
    },

    stt: {
      transcribe: async () => '',
      warmup: asyncNoop,
      status: async () => ({ available: false }),
      engine: async () => 'apple',
      downloadModel: async () => ({ error: 'no model' }),
      begin: noop,
      append: noop,
      finish: noop,
      cancel: noop,
      onPartial: on,
    },

    chat: {
      send: asyncNoop,
      abort: noop,
      // Real shape is { providers: [...], active: id, effective, reason } —
      // the Preferences assistant section iterates `chatInfo.providers`, so
      // an empty array (not a bare []) keeps it from throwing.
      providers: async () => ({ providers: [], active: null, effective: null, reason: null }),
      complete: async () => '',
      keySet: asyncNoop,
      providerSet: asyncNoop,
      providerDelete: asyncNoop,
      providerAdd: async () => ({ id: 'mock-provider' }),
      onDelta: on,
      onDone: on,
      onTool: on,
      onRequestyNotice: on,
    },

    brain: {
      open: asyncNoop,
      close: asyncNoop,
      index: async () => [],
      read: async () => null,
      write: asyncNoop,
      delete: asyncNoop,
      coreInfo: async () => ({}),
      promote: asyncNoop,
      onChanged: on,
    },

    review: { generate: asyncNoop },

    mentor: { onCheck: on, answer: asyncNoop, judge: async () => '' },

    lsp: {
      didOpen: noop,
      didChange: noop,
      didClose: noop,
      hover: async () => null,
      definition: async () => null,
      onDiagnostics: on,
      onMissing: on,
    },

    pickFolder: async () => null,
    pickFile: async () => null,

    app: { onBeforeQuit: on, quitReady: noop, onOpenPreferences: on },
    menu: { onAction: on },
    popout: { onCloseRequest: on, close: noop, supported: false },
  }
})()
