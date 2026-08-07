import { contextBridge, ipcRenderer } from 'electron'

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
  },
  store: {
    get: (key) => ipcRenderer.invoke('store:get', key),
    set: (key, value) => ipcRenderer.invoke('store:set', { key, value }),
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
  conductor: {
    allowRun: (v) => ipcRenderer.send('conductor:allowRun', v),
    onOpen: (cb) => ipcRenderer.on('conductor:open', (e, m) => cb(m)),
    onActed: (cb) => ipcRenderer.on('conductor:acted', (e, m) => cb(m)),
  },
  doc: {
    read: (p) => ipcRenderer.invoke('doc:read', p),
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
})
