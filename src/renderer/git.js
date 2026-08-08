// Git widget: branch chip in the topbar, working-tree stats, branch menu.
// The 5s poll is started by renderer.js only after the lock screen resolves,
// so the locked app isn't throwing gated IPC errors on a timer.
import { tome, toast, el } from './util.js'
import { wsState } from './state.js'
import { wireMenu, menuItem, menuInput, menuRule, menuLabel } from './menus.js'
import { addHistory } from './panes.js'

const gitChip = document.getElementById('git-chip')
const gitBranch = document.getElementById('git-branch')
const gitStats = document.getElementById('git-stats')

export async function refreshGit() {
  if (!wsState.activeRoot) {
    gitChip.classList.add('hidden')
    gitStats.textContent = ''
    return
  }
  const info = await tome.git.info(wsState.activeRoot)
  if (!info.repo) {
    gitChip.classList.add('hidden')
    gitStats.textContent = ''
    return
  }
  gitChip.classList.remove('hidden')
  gitBranch.textContent = info.branch
  gitChip.title = `Git — ${wsState.activeRoot}`
  gitStats.textContent = ''
  const stat = (n, cls, sym) => {
    if (!n) return
    gitStats.appendChild(el('span', cls, sym + n))
  }
  stat(info.added, 'g-add', '+')
  stat(info.modified, 'g-mod', '~')
  stat(info.deleted, 'g-del', '−')
  stat(info.ahead, 'g-sync', '↑')
  stat(info.behind, 'g-sync', '↓')
}

export function startGitPolling() {
  setInterval(refreshGit, 5000)
}

// Deferred, not top-level: menus.js imports this module, so at our evaluation
// time its `allMenus` const is still in TDZ and wireMenu() would throw,
// killing the whole renderer module graph. renderer.js calls this once the
// graph is loaded.
export function initGitMenu() {
  wireMenu('git-chip', 'git-menu', async (menu) => {
    menu.innerHTML = ''
    // the chip is hidden without a root, but a workspace deleted while it is
    // still on screen would land here — every entry below needs the path
    if (!wsState.activeRoot) return menuLabel(menu, 'no active folder')
    menuLabel(menu, wsState.activeRoot.split('/').pop())
    menuInput(menu, 'new branch from HEAD…', 'Create', async (name) => {
      const r = await tome.git.checkout(wsState.activeRoot, name, true)
      r.ok ? checkoutPulse(name) : toast(r.error)
    })
    menuRule(menu)
    menuItem(menu, { label: 'History', hint: 'commit log', onClick: addHistory })
    menuRule(menu)
    let branches = []
    try {
      branches = await tome.git.branches(wsState.activeRoot)
    } catch {}
    const current = gitBranch.textContent
    for (const b of branches) {
      menuItem(menu, {
        label: b,
        active: b === current,
        hint: b === current ? 'current' : '',
        onClick: async () => {
          if (b === current) return
          const r = await tome.git.checkout(wsState.activeRoot, b, false)
          r.ok ? checkoutPulse(b) : toast(r.error)
        },
      })
    }
  })
}

function checkoutPulse(branch) {
  gitBranch.textContent = branch
  gitChip.classList.remove('pulse')
  void gitChip.offsetWidth // restart animation
  gitChip.classList.add('pulse')
  toast(`Switched to ${branch}`, 'ok')
  refreshGit()
}
