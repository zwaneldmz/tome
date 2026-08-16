// Entry point: fan main-process events out to live panels, then boot
// (lock screen -> persisted state -> render). The pieces live in:
//   panels/   one file per dockview panel class
//   panes.js  the dockview grid + pane-opening actions + conductor bridge
//   menus.js  topbar menus (workspace, ＋)   tree.js  file tree sidebar
//   git.js    branch widget + polling        airgap-ui.js  strips + modals
//   modals.js modal shell   util.js  el()/toast/tome   state.js, regs.js
// Tauri IPC shim — must evaluate before anything else reads window.tome
// (util.js does so at module-eval time on the very next line).
import './tome-ipc.js'
import { tome, toast } from './util.js'
import { prefs, wsState, agState, counters } from './state.js'
import { terms, chats, brains } from './regs.js'
import { dock, addChat, addBrain, openFile, restoreLayout } from './panes.js'
import { markdownLangExt } from './cm-lang.js'
import { renderAll } from './menus.js'
import { startGitPolling, initGitMenu } from './git.js'
import { activeWorkspace, syncFolders } from './workspaces.js'
import { checkRepoAirgap } from './repo-airgap.js'
import { bootAuth } from './lock.js'
import { maybeShowOnboarding } from './onboarding.js'
import { bootTheme } from './theme.js'
import { bootChrome } from './chrome.js'
import { initVoice, voiceActive, VOICE_CHAT_ID } from './voice.js'
import { loadEditorPrefs, warmLanguages } from './panels/editor.js'
import './airgap-ui.js' // wires the air-gap event listeners + strip ticker
import './viibi.js' // the mascot: status-bar sprite + processing-state wiring
import './mentor.js' // mentor mode: gate subscription + per-workspace uq/verbose
import './keys.js' // the keyboard spine: pane keys, quick open, zoom, reference
import './menu-bridge.js' // native menu bar actions → the same functions the buttons use
import './style.css'

// ---------- pty / chat / brain fan-out ----------
tome.pty.onData(({ id, data }) => terms.get(id)?.write(data))
tome.pty.onExit(({ id, exitCode }) =>
  terms.get(id)?.write(`\r\n\x1b[2m[process exited ${exitCode}]\x1b[0m\r\n`)
)
// While the ambient voice session owns 'chat-voice' it renders the open
// transcript pane itself (voice.js drives bubble/appendDelta/toolNote/finish
// so history never forks) — fanning the same events out here too would
// double every delta. When voice is idle, typed sends in that pane come
// through this fan-out as usual.
const voiceOwns = (id) => id === VOICE_CHAT_ID && voiceActive()
tome.chat.onDelta(({ id, text }) => !voiceOwns(id) && chats.get(id)?.appendDelta(text))
tome.chat.onDone(
  ({ id, error, aborted }) => !voiceOwns(id) && chats.get(id)?.finish(error, aborted)
)
tome.chat.onTool(({ id, tool, hint }) => !voiceOwns(id) && chats.get(id)?.toolNote(tool, hint))
tome.brain.onChanged(({ ws: bws, index }) => brains.get(bws)?.onChanged(index))

// ---------- boot ----------
// Boot profiling: performance.now() deltas from module evaluation, printed
// once at boot end when main was launched with TOME_PROFILE=1 (preload
// mirrors the env var as tome.profile; main prints its own timeline).
const bootT0 = performance.now()
const bootMarks = []
const mark = (label) => bootMarks.push(`${label}: ${(performance.now() - bootT0).toFixed(0)}ms`)
mark('module evaluation start')

;(async () => {
  await bootTheme() // before the lock screen paints — store:get is open while locked
  mark('bootTheme done')
  await bootAuth(tome, toast) // main gates the sensitive IPC until this resolves
  mark('bootAuth done')
  await bootChrome()
  mark('bootChrome done')
  await initVoice() // topbar mic + chat-voice event listeners (inert until toggled)
  maybeShowOnboarding() // first run only — checks 'onboarded-v1' itself
  const saved = await tome.store.get('workspaces')
  if (saved && Array.isArray(saved.workspaces)) {
    wsState.ws = saved
    if (wsState.ws.active >= wsState.ws.workspaces.length)
      wsState.ws.active = wsState.ws.workspaces.length - 1
  }
  await loadEditorPrefs() // before restoreLayout, so reopened editors get them
  const agPref = await tome.store.get('airgap-default')
  if (agPref !== null) prefs.airgapDefault = !!agPref
  if (await tome.store.get('conductor-run')) {
    prefs.conductorRun = true
    tome.conductor.allowRun(true)
  }
  tome.airgap.state().then((s) => Object.assign(agState, s))
  syncFolders() // main starts with an empty confinement list
  wsState.activeRoot = activeWorkspace()?.folders[0] || null
  renderAll()
  try {
    await restoreLayout()
  } catch (err) {
    console.warn('layout restore failed:', err)
  }
  mark('restoreLayout done')
  if (tome.profile) console.log('[profile] renderer boot — ' + bootMarks.join(' | '))
  // ---- post-paint ----
  // Everything below is boot-tail work: nothing here is awaited by the paint
  // path above, so it all runs after the layout is on screen.
  // After bootAuth, so the lock-gated apply channel is reachable; a repo's
  // .tome/airgap.json still needs the user's consent before it is honored.
  checkRepoAirgap()
  initGitMenu() // deferred out of git.js's module body — see the note there
  startGitPolling() // gated on unlock: the IPC gate refuses while locked
  // Idle warm-up of the two lazy loads the user is most likely to hit next:
  // the brain pane's markdown mode and the editor's language table. Idle
  // time only — neither may compete with first paint.
  const idle = window.requestIdleCallback || ((fn) => setTimeout(fn, 1000))
  idle(() => {
    markdownLangExt()
    // warmLanguages is exported from editor.js, which is already statically
    // imported here (boot uses loadEditorPrefs) — call it directly; a
    // dynamic import() of an already-static module just confuses the bundler.
    warmLanguages()
  })
  // whisper-cli model warm-up for push-to-talk; main gates it on the
  // 'voice-warmup' store key (default off) and swallows all failures.
  tome.stt.warmup().catch(() => {})
  if (tome.shotMode && dock.panels.length === 0) {
    // screenshot/demo mode: open a representative set of panes against the
    // active workspace's first folder — only when no layout restored one
    // (a pre-seeded layout file drives the dedicated tour shots).
    // activeRoot starts null (it is only set by clicking a tree root), so
    // fall back through activeWorkspace — the same chain paneCwd() uses
    // for every real pane spawn.
    const root = wsState.activeRoot || activeWorkspace()?.folders[0]
    if (root) {
      const id = `pty-${++counters.seq}`
      dock.addPanel({
        id,
        component: 'terminal',
        title: `⛨ zsh — demo`,
        params: { ptyId: id, kind: 'terminal', cwd: root, airgap: true },
      })
      openFile(`${root}/package.json`)
      addChat()
      addBrain()
    }
  }
})()
