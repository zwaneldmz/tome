// File tree sidebar: workspace folders, lazy directory expansion, junk-dir
// dimming. Clicking a folder also makes it the git widget's root.
import { tome, el, toast } from './util.js'
import { wsState } from './state.js'
import { activeWorkspace, saveWs, syncAssistantRoot } from './workspaces.js'
import { openFile } from './panes.js'
import { refreshGit } from './git.js'

// renderAll/addFolderToActive/menuItem/floatingMenu live in menus.js with the
// workspace menu; the menus <-> tree import cycle is safe because neither
// side calls the other at module-evaluation time.
import { renderAll, addFolderToActive, menuItem, floatingMenu } from './menus.js'
import { confirmModal, promptModal } from './modals.js'
import { renderStatusbar } from './statusbar.js'
import { folderIcon, fileIcon, newFileIcon, newFolderIcon } from './icons.js'
import { validateRelPath } from './tree-create.js'
import { checkRepoEgress } from './repo-egress.js'

// Rows render into #tree-body; #tree itself keeps the header row (minimizer)
// pinned above the scroll.
const treeEl = document.getElementById('tree-body')
const JUNK_DIRS = new Set(['node_modules', 'out', 'dist', '.venv', '__pycache__', '.next', 'target'])

// #tree-head is static markup in index.html, not something renderTree()
// redraws — updateHeaderButtons() has to be called from everywhere
// wsState.activeRoot can change (see the call sites in setActiveRoot and
// renderTree below), or the buttons go stale.
const btnNewFile = document.getElementById('btn-new-file')
const btnNewFolder = document.getElementById('btn-new-folder')
btnNewFile.appendChild(newFileIcon())
btnNewFolder.appendChild(newFolderIcon())
function updateHeaderButtons() {
  const disabled = !wsState.activeRoot
  btnNewFile.disabled = disabled
  btnNewFolder.disabled = disabled
  btnNewFile.title = disabled ? 'needs a workspace folder' : 'New file'
  btnNewFolder.title = disabled ? 'needs a workspace folder' : 'New folder'
}
btnNewFile.addEventListener('click', () => createFileIn(wsState.activeRoot))
btnNewFolder.addEventListener('click', () => createFolderIn(wsState.activeRoot))

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
    const row = el('div', 'entry ' + (e.dir ? 'dir' : 'file') + (junk ? ' junk' : ''))
    const iconWrap = el('span', 'entry-icon')
    iconWrap.appendChild(e.dir ? folderIcon(false) : fileIcon())
    const label = el('span', 'entry-name', e.name)
    row.append(iconWrap, label)
    row.style.paddingLeft = 10 + depth * 13 + 'px'
    container.appendChild(row)
    if (e.dir) {
      let open = false
      let kids = null
      row.addEventListener('click', () => {
        setActiveRoot(rootPath)
        open = !open
        iconWrap.replaceChildren(folderIcon(open))
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
  // The tree drives activeRoot directly (no saveWs round-trip): the
  // assistant's root follows the click too.
  syncAssistantRoot()
  for (const h of treeEl.querySelectorAll('.root-head')) {
    h.classList.toggle('active', h.dataset.path === rootPath)
  }
  refreshGit()
  renderStatusbar()
  updateHeaderButtons()
  // A root the user clicks into may carry .tome/egress.json — re-check
  // (fire-and-forget; the consent store dedupes already-seen files).
  checkRepoEgress()
}

// Shared by the header buttons above (root = activeRoot), each root head's
// ＋ menu below (root = that head's folder), and the topbar/pane-group ＋
// menu (menus.js imports these — the safe cycle noted at the top of the file).
// `target` is only ever passed by the pane-group ＋ menu (the other two call
// sites have no pane-group context) and is forwarded to openFile so the new
// file lands as a tab in that group instead of panes.js's default placement.
export async function createFileIn(root, target) {
  if (!root) return
  const input = await promptModal('New file', 'name or path — e.g. src/util.js', '', 'Create')
  if (input == null) return // cancelled
  const check = validateRelPath(input)
  if (!check.ok) return toast(check.reason)
  const full = `${root}/${check.rel}`
  const slash = check.rel.lastIndexOf('/')
  if (slash !== -1) {
    const parentRel = check.rel.slice(0, slash)
    try {
      // Parent dirs first: createFile's exclusive 'wx' flag turns a missing
      // parent into ENOENT, so a nested path like "src/new/thing.js" needs its
      // folder made ahead of time — mkdir is recursive/idempotent, so an
      // already-there *directory* parent is fine. But if the parent segment is
      // itself a plain file, mkdir's target IS that segment and Node throws
      // EEXIST naming it exactly (verified: mkdir(recursive) on an existing
      // file path throws EEXIST for the exact path, ENOTDIR for a file deeper
      // in the chain) — caught here, separately from createFile's own EEXIST
      // below, so the toast blames the segment that actually collided instead
      // of claiming the full nested path "already exists" when it never did.
      await tome.fs.mkdir(`${root}/${parentRel}`)
    } catch (err) {
      if (String(err.message).includes('EEXIST'))
        toast(`“${parentRel}” is already a file — can't create “${check.rel}”`)
      else toast(`couldn't create “${check.rel}”: ${err.message}`)
      return
    }
  }
  try {
    await tome.fs.createFile(full)
  } catch (err) {
    if (String(err.message).includes('EEXIST')) toast(`“${check.rel}” already exists`)
    else toast(`couldn't create “${check.rel}”: ${err.message}`)
    return
  }
  // Full re-render is the house pattern (see the root × handler below) — it
  // loses every folder's expansion state, but the new file's parent folder
  // may not even be expanded yet, so there's nothing worth preserving here.
  renderTree()
  openFile(full, undefined, target)
}

// `target` is accepted only for call-site symmetry with createFileIn (the ＋
// menu in menus.js forwards it identically to both New file/New folder) — a
// folder opens no pane, so it's unused here.
export async function createFolderIn(root, target) {
  if (!root) return
  const input = await promptModal('New folder', 'name or path — e.g. src/lib', '', 'Create')
  if (input == null) return // cancelled
  const check = validateRelPath(input)
  if (!check.ok) return toast(check.reason)
  try {
    await tome.fs.mkdir(`${root}/${check.rel}`)
  } catch (err) {
    toast(`couldn't create “${check.rel}”: ${err.message}`)
    return
  }
  renderTree()
}

function emptyState(text, btnLabel, onClick) {
  const box = el('div', 'tree-empty')
  box.append(el('p', '', text), el('button', '', btnLabel))
  box.querySelector('button').addEventListener('click', onClick)
  treeEl.appendChild(box)
}

export function renderTree() {
  updateHeaderButtons()
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
    // ＋ here targets *this* root, not activeRoot — a mini menu instead of a
    // second modal-flow since createFileIn/createFolderIn already own that.
    const add = el('button', 'root-add', '＋')
    add.title = 'New file or folder here'
    add.setAttribute('aria-haspopup', 'true')
    add.setAttribute('aria-expanded', 'false')
    add.addEventListener('click', (e) => {
      e.stopPropagation() // else the head's click handler below also fires, reassigning activeRoot
      floatingMenu(add, (menu) => {
        menuItem(menu, { label: 'New file here', onClick: () => createFileIn(folder) })
        menuItem(menu, { label: 'New folder here', onClick: () => createFolderIn(folder) })
      })
    })
    const rm = el('button', 'root-rm', '×')
    rm.title = 'Remove folder from workspace'
    rm.addEventListener('click', async (e) => {
      e.stopPropagation()
      const ok = await confirmModal(
        `Remove “${folder.split('/').pop() || folder}” from “${w.name}”?`,
        'The folder stays on disk; it only leaves this workspace.',
        'Remove folder'
      )
      if (!ok) return
      w.folders = w.folders.filter((f) => f !== folder)
      if (wsState.activeRoot === folder) wsState.activeRoot = w.folders[0] || null
      saveWs()
      renderAll()
    })
    // Grouped so `.root-head`'s space-between puts one pair at the end, not
    // three items evenly spread with ＋ stranded mid-row.
    const actions = el('div', 'root-actions')
    actions.append(add, rm)
    head.append(label, actions)
    const kids = document.createElement('div')
    head.addEventListener('click', () => setActiveRoot(folder))
    treeEl.append(head, kids)
    renderDir(folder, kids, 0, folder)
  }
}
