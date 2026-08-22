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
  const calls = {
    ptyCreate: [],
    graphifyStatus: [],
    graphifyBuild: [],
    graphifyQuery: [],
    graphifyPath: [],
    graphifyExplain: [],
    graphifyAffected: [],
    opencodeStatus: 0,
    opencodeKeySet: [],
    opencodeSetModel: [],
    opencodeModels: [],
    conductorSetCwd: [],
    chatHistoryList: [],
    chatSend: [],
  }

  const noop = () => {}
  // Event subscribers are RECORDED, not discarded: `window.__tomeMock.emit`
  // replays a payload to every subscriber of that event, so specs can drive
  // push-style features (the plan tracker, chat chips, mentor gates) with
  // the exact payloads the real backend emits. The unsubscribe returned to
  // the bridge stays a no-op — no spec has needed it yet.
  const handlers = {}
  const on = (event, cb) => {
    if (typeof cb === 'function') (handlers[event] ||= []).push(cb)
    return () => {}
  }
  // A subscription FACTORY bound to one wire event — the real bridge does
  // this binding at each key (`onTool: (cb) => on('chat:tool', cb)`), so the
  // mock has to spell it out too.
  const sub = (event) => (cb) => on(event, cb)
  const asyncNoop = async () => {}

  window.__tomeMock = {
    store,
    calls,
    emit: (event, payload) => (handlers[event] || []).forEach((cb) => cb(payload)),
    graphify: { available: true, built: false },
    // Mutable egress snapshot — `tome.egress.state()` returns it and specs
    // can seed panes (e.g. a low-confinement rung-2 state) by assigning
    // `window.__tomeMock.egress` before boot or emitting egress:state.
    egress: {
      panes: {},
      defaultMinutes: 15,
      repo: [],
      auth: { configured: false, totp: false },
    },
    opencode: {
      installed: true,
      version: '1.18.19',
      reason: null,
      auth: [{ id: 'deepseek', cred_type: 'api' }],
      providers: ['deepseek', 'eurouter'],
      providers_with_key: [],
      default_model: null,
    },
  }

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
      onExit: sub('pty:exit'),
    },

    fs: {
      readDir: async () => [],
      readFile: async () => null,
      writeFile: asyncNoop,
      mkdir: asyncNoop,
      createFile: asyncNoop,
      watch: asyncNoop,
      unwatch: asyncNoop,
      onChanged: sub('fs:changed'),
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
      // A no-repo shape rather than `null` so workspace-seeding specs
      // (graphify) don't make refreshGit throw on `.repo`.
      info: async () => ({ repo: false }),
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

    // Workspace knowledge graph — the pane reads availability/built state
    // from `window.__tomeMock.graphify` so specs can flip it per test.
    graphify: {
      status: async (ws) => {
        calls.graphifyStatus.push(ws)
        const g = window.__tomeMock.graphify
        const out = ws + '/graphify-out'
        return {
          available: g.available,
          version: g.available ? 'graphify 0.9.48' : null,
          reason: g.available ? null : 'graphify not found on PATH (No such file or directory (os error 2))',
          built: g.built,
          out_dir: out,
          graph_json: out + '/graph.json',
          graph_html: out + '/graph.html',
          report: out + '/GRAPH_REPORT.md',
        }
      },
      build: async (ws, onLine) => {
        calls.graphifyBuild.push(ws)
        onLine('graphify — building the workspace graph')
        onLine('[1/2] extracting code with tree-sitter (offline, no LLM)')
        onLine('[2/2] clustering communities and writing report + graph.html')
        window.__tomeMock.graphify.built = true
        return { summary: 'graph built — ' + ws + '/graphify-out/graph.json' }
      },
      cancel: async () => ({ killed: false }),
      query: async (ws, question) => {
        calls.graphifyQuery.push({ ws, question })
        return 'query result'
      },
      path: async (ws, from, to) => {
        calls.graphifyPath.push({ ws, from, to })
        return 'path result'
      },
      explain: async (ws, symbol) => {
        calls.graphifyExplain.push({ ws, symbol })
        return 'explain result'
      },
      affected: async (ws, symbol) => {
        calls.graphifyAffected.push({ ws, symbol })
        return 'affected result'
      },
    },

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
      setCwd: (root) => {
        calls.conductorSetCwd.push(root)
        return Promise.resolve({})
      },
      onReadRequest: sub('conductor:readRequest'),
      onOpen: sub('conductor:open'),
      onActed: sub('conductor:acted'),
      onAgent: sub('conductor:agent'),
    },

    doc: { readBytes: async () => ({ base64: '' }) },

    theme: { set: noop },
    openPath: asyncNoop,

    egress: {
      state: async () => window.__tomeMock.egress,
      unlock: async () => ({ ok: true }),
      relock: asyncNoop,
      setup: async () => ({ ok: true }),
      enrollTotp: async () => ({ secret: '', uri: '' }),
      confirmTotp: async () => true,
      readRepo: async () => ({ state: 'absent' }),
      consentRepo: async () => ({ ok: true, applied: [], rejected: [] }),
      revokeRepo: async () => ({ ok: true }),
      onBlocked: sub('egress:blocked'),
      onState: sub('egress:state'),
    },

    agents: {
      list: async () => [
        { name: 'claude', available: true, custom: false },
        { name: 'opencode', available: true, custom: false },
      ],
      customs: async () => [],
      changed: noop,
    },

    // opencode CLI config — status shape mirrors ipc/opencode.rs (types
    // only, never keys); keySet/models/setModel record into calls.
    opencode: {
      status: async () => {
        calls.opencodeStatus++
        return window.__tomeMock.opencode
      },
      keySet: async (provider, key) => {
        calls.opencodeKeySet.push({ provider, key })
      },
      models: async () => calls.opencodeModels || [],
      setModel: async (model) => {
        calls.opencodeSetModel.push(model)
      },
    },

    events: { list: async () => [], onAppended: sub('events:appended') },

    runs: {
      start: asyncNoop,
      cancel: asyncNoop,
      list: async () => [],
      onChanged: sub('runs:changed'),
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
      onPartial: sub('stt:partial'),
    },

    chat: {
      send: async (id, messages) => {
        calls.chatSend.push({ id, messages })
      },
      abort: noop,
      // Real shape is { providers: [...], active: id, effective, reason,
      // none } — the Preferences assistant section iterates
      // `chatInfo.providers`, so an empty array (not a bare []) keeps it
      // from throwing. `none: true` is a fresh profile: no provider picked
      // yet (the P3.1 no-default state the chat pane's send gate keys on).
      providers: async () => ({ providers: [], active: null, effective: null, reason: null, none: true }),
      complete: async () => '',
      keySet: asyncNoop,
      providerSet: asyncNoop,
      providerDelete: asyncNoop,
      providerAdd: async () => ({ id: 'mock-provider' }),
      onDelta: sub('chat:delta'),
      onDone: sub('chat:done'),
      onTool: sub('chat:tool'),
      onToolDone: sub('chat:tool-done'),
      onRequestyNotice: sub('chat:requesty-notice'),
      // Searchable archive — specs seed `window.__tomeMock.history` with
      // the entries each call returns.
      historyList: async (query) => {
        calls.chatHistoryList.push(query)
        return window.__tomeMock.history || []
      },
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
      onChanged: sub('brain:changed'),
    },

    review: { generate: asyncNoop },

    mentor: { onCheck: sub('mentor:check'), answer: asyncNoop, judge: async () => '' },

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
