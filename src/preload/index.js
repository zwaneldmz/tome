import { contextBridge, ipcRenderer, webUtils } from 'electron'

contextBridge.exposeInMainWorld('tome', {
  home: ipcRenderer.sendSync('app:home'),
  shotMode: !!process.env.TOME_SHOT,
  pty: {
    create: (opts) => ipcRenderer.invoke('pty:create', opts),
    write: (id, data) => ipcRenderer.send('pty:write', { id, data }),
    resize: (id, cols, rows) => ipcRenderer.send('pty:resize', { id, cols, rows }),
    kill: (id) => ipcRenderer.send('pty:kill', { id }),
    onData: (cb) => ipcRenderer.on('pty:data', (e, m) => cb(m)),
    onExit: (cb) => ipcRenderer.on('pty:exit', (e, m) => cb(m)),
  },
  fs: {
    readDir: (p) => ipcRenderer.invoke('fs:readDir', p),
    readFile: (p) => ipcRenderer.invoke('fs:readFile', p),
    writeFile: (path, content) => ipcRenderer.invoke('fs:writeFile', { path, content }),
    // an open editor asks to hear about changes made outside the app
    watch: (p) => ipcRenderer.invoke('fs:watch', p),
    unwatch: (p) => ipcRenderer.invoke('fs:unwatch', p),
    onChanged: (cb) => ipcRenderer.on('fs:changed', (e, p) => cb(p)),
    // Prettier lives in main; returns formatted text, null (no parser), or
    // { error } when the file does not currently parse
    format: (path, content) => ipcRenderer.invoke('fmt:format', { path, content }),
  },
  store: {
    get: (key) => ipcRenderer.invoke('store:get', key),
    set: (key, value) => ipcRenderer.invoke('store:set', { key, value }),
  },
  webUtils: {
    // File.path is gone in newer Electron; drag-and-drop resolves the
    // absolute path of a dropped File through here instead.
    pathForFile: (file) => webUtils.getPathForFile(file),
  },
  git: {
    info: (dir) => ipcRenderer.invoke('git:info', dir),
    branches: (dir) => ipcRenderer.invoke('git:branches', dir),
    checkout: (dir, branch, create) => ipcRenderer.invoke('git:checkout', { dir, branch, create }),
    log: (dir, limit) => ipcRenderer.invoke('git:log', { dir, limit }),
    commit: (dir, hash) => ipcRenderer.invoke('git:commit', { dir, hash }),
    diff: (dir, hash, file) => ipcRenderer.invoke('git:diff', { dir, hash, file }),
  },
  auth: {
    status: () => ipcRenderer.invoke('auth:status'),
    login: (opts) => ipcRenderer.invoke('auth:login', opts),
    touchid: () => ipcRenderer.invoke('auth:touchid'),
  },
  panes: {
    sync: (list) => ipcRenderer.send('panes:sync', list),
  },
  ws: {
    // keeps main's open-folder confinement list in sync with the workspace state
    syncFolders: (folders) => ipcRenderer.send('ws:sync', folders),
  },
  conductor: {
    allowRun: (v) => ipcRenderer.send('conductor:allowRun', v),
    onOpen: (cb) => ipcRenderer.on('conductor:open', (e, m) => cb(m)),
    onActed: (cb) => ipcRenderer.on('conductor:acted', (e, m) => cb(m)),
  },
  doc: {
    read: (p) => ipcRenderer.invoke('doc:read', p),
  },
  theme: {
    // resolved appearance ('light' | 'dark') — main uses it for window
    // backgrounds and the CSS it injects into converted-document iframes
    set: (pref, mode) => ipcRenderer.send('theme:set', { pref, mode }),
  },
  openPath: (p) => ipcRenderer.invoke('shell:openPath', p),
  airgap: {
    state: () => ipcRenderer.invoke('airgap:state'),
    unlock: (opts) => ipcRenderer.invoke('airgap:unlock', opts),
    relock: (paneId) => ipcRenderer.invoke('airgap:relock', paneId),
    setup: (passphrase) => ipcRenderer.invoke('airgap:setup', { passphrase }),
    enrollTotp: () => ipcRenderer.invoke('airgap:enrollTotp'),
    confirmTotp: (code) => ipcRenderer.invoke('airgap:confirmTotp', { code }),
    onBlocked: (cb) => ipcRenderer.on('airgap:blocked', (e, m) => cb(m)),
    onState: (cb) => ipcRenderer.on('airgap:state', (e, m) => cb(m)),
  },
  agents: {
    list: () => ipcRenderer.invoke('agents:list'),
  },
  chat: {
    send: (id, messages, brainWs) => ipcRenderer.invoke('chat:send', { id, messages, brainWs }),
    abort: (id) => ipcRenderer.send('chat:abort', id),
    onDelta: (cb) => ipcRenderer.on('chat:delta', (e, m) => cb(m)),
    onDone: (cb) => ipcRenderer.on('chat:done', (e, m) => cb(m)),
    onTool: (cb) => ipcRenderer.on('chat:tool', (e, m) => cb(m)),
  },
  brain: {
    open: (ws) => ipcRenderer.invoke('brain:open', { ws }),
    close: (ws) => ipcRenderer.invoke('brain:close', { ws }),
    index: (ws) => ipcRenderer.invoke('brain:index', { ws }),
    read: (ws, rel) => ipcRenderer.invoke('brain:read', { ws, rel }),
    write: (ws, rel, content, exclusive) =>
      ipcRenderer.invoke('brain:write', { ws, rel, content, exclusive }),
    delete: (ws, rel) => ipcRenderer.invoke('brain:delete', { ws, rel }),
    coreInfo: () => ipcRenderer.invoke('brain:coreInfo'),
    promote: (ws, rel, folder, overwrite, rename) =>
      ipcRenderer.invoke('brain:promote', { ws, rel, folder, overwrite, rename }),
    onChanged: (cb) => ipcRenderer.on('brain:changed', (e, m) => cb(m)),
  },
  pickFolder: () => ipcRenderer.invoke('dialog:pickFolder'),
  pickFile: () => ipcRenderer.invoke('dialog:pickFile'),
  app: {
    onBeforeQuit: (cb) => ipcRenderer.on('app:before-quit', () => cb()),
    quitReady: () => ipcRenderer.send('app:quit-ready'),
    onOpenPreferences: (cb) => ipcRenderer.on('app:open-preferences', () => cb()),
  },
  // Native menu bar: one generic channel, the renderer's menu-bridge
  // switches on action.id.
  menu: {
    onAction: (cb) => ipcRenderer.on('menu:action', (e, action) => cb(action)),
  },
  // A popped-out window is trying to close. Main holds it open until close()
  // is called; never calling it leaves the window where it is.
  popout: {
    onCloseRequest: (cb) => ipcRenderer.on('popout:close-request', (e, req) => cb(req)),
    close: (id) => ipcRenderer.invoke('popout:close', id),
  },
})
