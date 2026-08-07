// File tree sidebar: workspace folders, lazy directory expansion, junk-dir
// dimming. Clicking a folder also makes it the git widget's root.
import { tome, el } from './util.js'
import { wsState } from './state.js'
import { activeWorkspace, saveWs } from './workspaces.js'
import { openFile } from './panes.js'
import { refreshGit } from './git.js'

// renderAll/addFolderToActive live in menus.js with the workspace menu;
// the menus <-> tree import cycle is safe because neither side calls the
// other at module-evaluation time.
import { renderAll, addFolderToActive } from './menus.js'

const treeEl = document.getElementById('tree')
const JUNK_DIRS = new Set(['node_modules', 'out', 'dist', '.venv', '__pycache__', '.next', 'target'])

async function renderDir(dir, container, depth, rootPath) {
  let entries
  try {
    entries = await tome.fs.readDir(dir)
  } catch {
    return
  }
  for (const e of entries) {
    const full = `${dir}/${e.name}`
    const junk = e.dir && JUNK_DIRS.has(e.name)
    const row = el(
      'div',
      'entry ' + (e.dir ? 'dir' : 'file') + (junk ? ' junk' : ''),
      (e.dir ? '▸ ' : '') + e.name
    )
    row.style.paddingLeft = 10 + depth * 13 + 'px'
    container.appendChild(row)
    if (e.dir) {
      let open = false
      let kids = null
      row.addEventListener('click', () => {
        setActiveRoot(rootPath)
        open = !open
        row.textContent = (open ? '▾ ' : '▸ ') + e.name
        if (open && !kids) {
          kids = document.createElement('div')
          row.after(kids)
          renderDir(full, kids, depth + 1, rootPath)
        } else if (kids) {
          kids.style.display = open ? '' : 'none'
        }
      })
    } else {
      row.addEventListener('click', () => {
        setActiveRoot(rootPath)
        openFile(full)
      })
    }
  }
}

function setActiveRoot(rootPath) {
  if (wsState.activeRoot === rootPath) return
  wsState.activeRoot = rootPath
  for (const h of treeEl.querySelectorAll('.root-head')) {
    h.classList.toggle('active', h.dataset.path === rootPath)
  }
  refreshGit()
}

function emptyState(text, btnLabel, onClick) {
  const box = el('div', 'tree-empty')
  box.append(el('p', '', text), el('button', '', btnLabel))
  box.querySelector('button').addEventListener('click', onClick)
  treeEl.appendChild(box)
}

export function renderTree() {
  treeEl.innerHTML = ''
  const w = activeWorkspace()
  if (!w) {
    emptyState('A workspace groups the folders you are working across.', '▚ New workspace', () =>
      document.getElementById('ws-chip').click()
    )
    return
  }
  if (!w.folders.length) {
    emptyState(`“${w.name}” has no folders yet.`, '＋ Add folder', addFolderToActive)
    return
  }
  for (const folder of w.folders) {
    const head = el('div', 'root-head' + (folder === wsState.activeRoot ? ' active' : ''))
    head.dataset.path = folder
    const label = el('span', '', folder.split('/').pop() || folder)
    label.title = folder
    const rm = el('button', 'root-rm', '×')
    rm.title = 'Remove folder from workspace'
    rm.addEventListener('click', (e) => {
      e.stopPropagation()
      w.folders = w.folders.filter((f) => f !== folder)
      if (wsState.activeRoot === folder) wsState.activeRoot = w.folders[0] || null
      saveWs()
      renderAll()
    })
    head.append(label, rm)
    const kids = document.createElement('div')
    head.addEventListener('click', () => setActiveRoot(folder))
    treeEl.append(head, kids)
    renderDir(folder, kids, 0, folder)
  }
}
