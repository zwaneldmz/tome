import { contextBridge, ipcRenderer } from 'electron'

contextBridge.exposeInMainWorld('tome', {
  home: process.env.HOME || '/',
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
