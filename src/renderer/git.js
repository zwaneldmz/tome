// Git widget: branch chip in the topbar, working-tree stats, branch menu.
// The 5s poll is started by renderer.js only after the lock screen resolves,
// so the locked app isn't throwing gated IPC errors on a timer.
import { tome, toast, el } from './util.js'
import { wsState } from './state.js'
import { wireMenu, menuItem, menuInput, menuRule, menuLabel } from './menus.js'
import { addHistory } from './panes.js'
import { modalShell } from './modals.js'
import { mentorState } from './mentor.js'

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
    menuItem(menu, { label: 'Commit…', onClick: commitFlow })
    menuItem(menu, { label: 'Push', onClick: pushFlow })
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

async function commitFlow() {
  const dir = wsState.activeRoot
  if (!dir) return toast('no active folder')
  let files
  try {
    files = (await tome.git.status(dir)).files
  } catch (err) {
    return toast(`git status failed: ${err.message}`)
  }
  const m = modalShell(`Commit — ${dir.split('/').pop()}`)
  m.err.remove() // report via toasts
  const list = el('div', 'commit-files')
  for (const f of files) {
    const row = el('div', 'commit-file')
    row.append(el('span', 'commit-code', `${f.x}${f.y}`), el('span', '', f.path))
    list.appendChild(row)
  }
  m.body.appendChild(list)
  const input = m.input('commit message', 'text')
  m.button('Stage all & commit', async () => {
    const message = input.value.trim()
    if (!message) return
    const doCommit = async () => {
      try {
        await tome.git.stage(dir, null)
        const r = await tome.git.commitCreate(dir, message)
        if (r.ok) {
          toast(`committed ${r.hash.slice(0, 7)}`, 'ok')
          m.close()
          refreshGit()
        } else {
          toast(r.error || 'nothing to commit')
        }
      } catch (err) {
        toast(err.message)
      }
    }
    comprehensionGate('commit', doCommit)
  })
  m.button('Cancel', () => m.close(), 'ghost')
}

async function pushFlow() {
  const dir = wsState.activeRoot
  if (!dir) return toast('no active folder')
  comprehensionGate('push', async () => {
    try {
      const r = await tome.git.push(dir)
      r.ok ? toast('pushed', 'ok') : toast(r.error)
    } catch (err) {
      toast(err.message)
    }
  })
}

// The deterministic half of the comprehension gate: before commit or push,
// when the mentor gate is armed at that point, ask the user to put the change
// into their own words. Skip (bottom-left) and a non-empty answer both let the
// action through; Cancel blocks it. Free-text is the fixed self-check here —
// the model-driven half of the gate (verbose mode's gate_question) already
// handles tailored questions with LLM-judged free text.
function comprehensionGate(verb, onProceed) {
  if (!mentorState.gate || !mentorState.gatePoints?.[verb]) return onProceed()
  const m = modalShell(`Before you ${verb}`)
  const box = m.body.parentElement
  const overlay = box.parentElement
  m.note(`In one sentence, what does this ${verb} do and why is it right?`)
  const ta = el('textarea', 'mentor-input')
  ta.rows = 3
  ta.spellcheck = false
  m.body.appendChild(ta)
  let skipped = false
  const proceed = () => {
    m.close()
    onProceed()
  }
  m.button(verb === 'commit' ? 'Commit anyway' : 'Push anyway', () => {
    if (ta.value.trim() || skipped) proceed()
    else toast('write a line first, or skip the test')
  })
  m.button('Cancel', () => m.close(), 'ghost')
  const skip = el('button', 'mentor-skip', 'Skip test')
  skip.type = 'button'
  skip.addEventListener('click', () => {
    skipped = true
    proceed()
  })
  overlay.appendChild(skip)
}
