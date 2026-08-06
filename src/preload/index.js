import { contextBridge, ipcRenderer } from 'electron'

contextBridge.exposeInMainWorld('tome', {
  home: process.env.HOME || '/',
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
    send: (id, messages) => ipcRenderer.invoke('chat:send', { id, messages }),
    onDelta: (cb) => ipcRenderer.on('chat:delta', (e, m) => cb(m)),
    onDone: (cb) => ipcRenderer.on('chat:done', (e, m) => cb(m)),
  },
  pickFolder: () => ipcRenderer.invoke('dialog:pickFolder'),
  pickFile: () => ipcRenderer.invoke('dialog:pickFile'),
})
