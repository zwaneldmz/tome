// Preferences (⌘,): one modal that surfaces the settings that were scattered
// across menus and key chords. Every control reads and writes the same
// persisted keys the original surfaces use, so nothing drifts out of sync.
import { tome, el, toast } from './util.js'
import { prefs } from './state.js'
import { modalShell, confirmModal } from './modals.js'
import { setTheme, themeState, THEME_ORDER, THEME_GLYPH } from './theme.js'
import { TERM_FONT, setTermFontSize } from './panels/terminal.js'
import { editorPrefs, setEditorPrefs } from './panels/editor.js'
import { totpModal } from './egress-ui.js'
import { showOnboarding } from './onboarding.js'
import { activeWorkspace } from './workspaces.js'
import { mentorState, saveMentorSettings, setUq, uq } from './mentor.js'
import { GROUPS, normalize, filterRows, sectionToGroup } from './settings-nav.js'

const THEME_LABEL = { system: 'Match system', light: 'Light', dark: 'Dark' }
const SIDEBAR_DEFAULT = 236

// ---- custom agents ----
// A renderer-side MIRROR of the vet rules in src/main/lib/custom-agents.js,
// duplicated here so the form can reject inline instead of round-tripping a
// bad entry into the store. The mirror is convenience, never authority:
// main re-vets every entry on every read of 'custom-agents', so a stale or
// bypassed copy of these regexes degrades to "entry silently missing from
// the ＋ menu", never to a command line.
const AGENT_ID_RE = /^[a-z0-9][a-z0-9-]{0,31}$/
const AGENT_BIN_RE = /^[a-z0-9][a-z0-9._-]{0,63}$/i
const AGENT_MODELFLAG_RE = /^--[a-z-]{2,20}$/
const AGENT_ARG_BAD_RE = /[^\x20-\x7e]|[;&|`$<>"'\\\s]/
const AGENT_RESERVED = new Set([
  'claude',
  'opencode',
  'pi',
  'terminal',
  'chat',
  'brain',
  'flow',
  'runs',
  'doc',
  'editor',
  'events',
])

// Inline vet of one form entry → error string or null. Kept in lockstep
// with vetCustomAgent's messages so a main-side refusal (visible as a
// silently dropped row) is debuggable against what the form said.
function vetAgentDraft({ id, label, bin, args, modelFlag }) {
  if (!AGENT_ID_RE.test(id)) return 'id: 1–32 chars of [a-z0-9-], starting with a letter or digit'
  if (AGENT_RESERVED.has(id)) return `id "${id}" is a built-in pane kind`
  if (!label || label.length > 40 || !/^[\x20-\x7e]+$/.test(label)) return 'label: 1–40 chars of printable ASCII'
  if (!AGENT_BIN_RE.test(bin)) return 'bin: a bare command name (no path separators)'
  if (args.length > 8) return 'args: at most 8 tokens'
  for (const a of args)
    if (!a || a.length > 64 || AGENT_ARG_BAD_RE.test(a))
      return 'args: single inert tokens (≤64 chars, no spaces or shell metacharacters)'
  if (modelFlag && !AGENT_MODELFLAG_RE.test(modelFlag)) return 'model flag: like --model (/--[a-z-]{2,20}/)'
  return null
}

// The "Agents" Preferences section: the user's custom CLIs as rows (label,
// bin, args, availability dot) plus an add-form. Persists through the same
// 'custom-agents' store key main reads, and nudges main afterwards so the
// conductor's kind descriptions refresh in-session. Exported because WS-E
// onboarding mounts the same section on its own surface.
export async function buildAgentsSection() {
  const section = el('section', 'prefs-section')
  section.append(el('h4', '', 'Agents'))
  // The store is the single writer/reader contract: load once, keep one
  // list, write it back whole on every mutation. main re-vets on read, so
  // persisting exactly what the form vetted is belt, not the suspenders.
  let customs = (await tome.agents.customs()) || []
  const listed = await tome.agents.list()
  const available = new Map(listed.filter((a) => a.custom).map((a) => [a.name, a.available]))

  // Enabled/disabled picker — the same 'agents-disabled' key the onboarding
  // wizard's Agents step writes and the ＋ menu reads. Built-ins first, then
  // customs; an unavailable CLI can be toggled here (it simply offers
  // itself grayed-out when it lands back in the menu).
  const disabled = new Set((await tome.store.get('agents-disabled')) || [])
  if (listed.length) {
    for (const a of listed) {
      const sw = el('button', 'prefs-switch')
      sw.type = 'button'
      sw.setAttribute('role', 'switch')
      sw.append(el('span', 'prefs-knob'))
      const paint = () => {
        sw.classList.toggle('on', !disabled.has(a.name))
        sw.setAttribute('aria-checked', String(!disabled.has(a.name)))
      }
      sw.addEventListener('click', () => {
        disabled.has(a.name) ? disabled.delete(a.name) : disabled.add(a.name)
        tome.store.set('agents-disabled', [...disabled])
        paint()
      })
      const r = row(section, a.label || a.name, sw, a.custom ? `${a.name} · custom` : a.available ? null : 'not installed')
      const dot = el('span', 'prefs-agent-dot' + (a.available ? ' on' : ''))
      dot.title = a.available ? `${a.bin || a.name} found on PATH` : 'not found on PATH'
      r.querySelector('.prefs-label').prepend(dot)
      paint()
    }
  }

  const persist = async () => {
    await tome.store.set('custom-agents', customs)
    tome.agents.changed() // conductor + agents:list re-read on next use
  }

  const list = el('div', 'prefs-agents')
  const renderRows = () => {
    list.innerHTML = ''
    if (!customs.length) {
      const empty = el('div', 'prefs-hint', 'No custom agents yet — add one below.')
      empty.style.padding = '4px 0'
      list.appendChild(empty)
    }
    for (const a of customs) {
      const r = el('div', 'prefs-row')
      const text = el('div', 'prefs-text')
      const head = el('span', 'prefs-label')
      const dot = el('span', 'prefs-agent-dot' + (available.get(a.id) ? ' on' : ''))
      dot.title = available.get(a.id) ? `${a.bin} found on PATH` : `${a.bin} not found on PATH`
      head.append(dot, a.label)
      text.append(head)
      text.append(
        el(
          'span',
          'prefs-hint',
          `${a.id} · ${[a.bin, ...(a.args || [])].join(' ')}${a.modelFlag ? ` · ${a.modelFlag}` : ''}`
        )
      )
      const remove = el('button', 'ag-btn ghost', 'Remove')
      remove.type = 'button'
      remove.addEventListener('click', async () => {
        customs = customs.filter((c) => c.id !== a.id)
        await persist()
        renderRows()
        toast(`removed ${a.label}`, 'ok')
      })
      r.append(text, remove)
      list.appendChild(r)
    }
  }
  renderRows()
  section.appendChild(list)

  // add-form: four small inputs + Add, inline errors under it
  const form = el('div', 'prefs-agent-form')
  const mk = (placeholder) => {
    const i = el('input')
    i.type = 'text'
    i.placeholder = placeholder
    i.setAttribute('aria-label', placeholder)
    i.spellcheck = false
    return i
  }
  const idIn = mk('id — e.g. aider')
  const labelIn = mk('label — e.g. Aider')
  const binIn = mk('bin — e.g. aider')
  const argsIn = mk('args · model flag')
  const err = el('div', 'prefs-hint')
  err.style.color = 'var(--danger, #e5534b)'
  const add = el('button', 'ag-btn ghost', 'Add')
  add.type = 'button'
  add.addEventListener('click', async () => {
    // The last field carries the two optional values space-separated:
    // arg tokens first, a trailing --flag as the model flag. Splitting here
    // is exactly the join the spawn builder performs, so what you type is
    // what the command line gets.
    const extras = argsIn.value.trim().split(/\s+/).filter(Boolean)
    const modelFlag = extras.find((t) => t.startsWith('--')) || ''
    const draft = {
      id: idIn.value.trim(),
      label: labelIn.value.trim(),
      bin: binIn.value.trim(),
      args: extras.filter((t) => t !== modelFlag),
      ...(modelFlag ? { modelFlag } : {}),
    }
    err.textContent = ''
    const bad = vetAgentDraft(draft)
    if (bad) {
      err.textContent = bad
      return
    }
    if (customs.some((c) => c.id === draft.id)) {
      err.textContent = `id "${draft.id}" is already in the list`
      return
    }
    customs = [...customs, draft]
    await persist()
    renderRows()
    idIn.value = labelIn.value = binIn.value = argsIn.value = ''
    toast(`added ${draft.label} — reopen the ＋ menu to spawn it`, 'ok')
  })
  form.append(idIn, labelIn, binIn, argsIn, add)
  section.appendChild(form)
  section.appendChild(err)
  return section
}

// ---- export destinations ----
// Consent-gated remote/local targets a finished run's promoted products can
// be copied to (panels/runs.js's Export… button). Every record here is an
// `export-destinations.json` entry main hashed at consent time
// (export_consent, src-tauri/src/export.rs) — this section only ever lists
// what main already verified and only ever adds through that same consent
// path, so a destination shown here can never disagree with what main
// actually uses.
export async function buildExportSection(closePreferences) {
  const section = el('section', 'prefs-section')
  section.append(el('h4', '', 'Export destinations'))
  const list = el('div', 'prefs-agents')
  section.appendChild(list)

  const renderRows = async () => {
    list.innerHTML = ''
    let destinations = []
    try {
      destinations = await tome.exportDest.list()
    } catch (err) {
      list.appendChild(el('div', 'prefs-hint', err.message))
      return
    }
    if (!destinations.length) {
      const empty = el('div', 'prefs-hint', 'No export destinations yet — add one below.')
      empty.style.padding = '4px 0'
      list.appendChild(empty)
    }
    for (const d of destinations) {
      const r = el('div', 'prefs-row')
      const text = el('div', 'prefs-text')
      text.append(el('span', 'prefs-label', d.label))
      text.append(
        el(
          'span',
          'prefs-hint',
          d.kind === 'http'
            ? `${d.method} ${d.url}${d.authBearer ? ' · authenticated' : ''}`
            : `${d.tool} · ${d.target}`
        )
      )
      const remove = el('button', 'ag-btn ghost', 'Remove')
      remove.type = 'button'
      remove.addEventListener('click', async () => {
        await tome.exportDest.revoke(d.id)
        await renderRows()
        toast(`removed ${d.label}`, 'ok')
      })
      r.append(text, remove)
      list.appendChild(r)
    }
  }
  await renderRows()

  const add = el('button', 'ag-btn ghost', 'Add destination…')
  add.type = 'button'
  add.addEventListener('click', () => {
    // modalShell keeps exactly one overlay at a time (its own doc comment)
    // — a nested form has to take Preferences' place, same as "Enroll
    // authenticator" / "Replay setup wizard" below already do.
    closePreferences?.()
    openAddDestinationModal()
  })
  section.appendChild(add)
  return section
}

// Custom multi-field form built directly on modalShell (the shared
// prompt/choice/confirm helpers each cover a single value, not a form this
// shaped) — the same field(label, control) idiom panels/flow.js's node
// editor modal uses. Submitting restates exactly what is consented to
// in a separate confirmModal before ever calling export_consent — the same
// "state it back before granting" discipline repo-egress.js's consentModal
// applies to the repo-allowlist flow, except here the content being
// consented to is fresh user input rather than something main already read
// and hashed, so the restatement is the only guard against a typo'd
// URL/target.
function openAddDestinationModal() {
  const m = modalShell('Add export destination')
  const field = (label, control) => {
    m.body.appendChild(el('label', 'flow-field-label', label))
    m.body.appendChild(control)
    return control
  }

  const kindSelect = field('Kind', el('select'))
  for (const [value, text] of [
    ['http', 'HTTP'],
    ['sftp', 'SFTP'],
  ]) {
    const opt = el('option', null, text)
    opt.value = value
    kindSelect.appendChild(opt)
  }

  const labelInput = field('Label', el('input'))
  labelInput.type = 'text'
  labelInput.placeholder = 'e.g. Staging bucket'

  const httpGroup = el('div', 'prefs-export-group')
  const urlInput = el('input')
  urlInput.type = 'text'
  urlInput.placeholder = 'https://example.com/uploads'
  httpGroup.append(el('label', 'flow-field-label', 'URL'), urlInput)
  const methodSelect = el('select')
  for (const v of ['PUT', 'POST']) {
    const opt = el('option', null, v)
    opt.value = v
    methodSelect.appendChild(opt)
  }
  httpGroup.append(el('label', 'flow-field-label', 'Method'), methodSelect)
  const authInput = el('input')
  authInput.type = 'password'
  authInput.placeholder = 'bearer token (optional)'
  httpGroup.append(el('label', 'flow-field-label', 'Authorization'), authInput)
  m.body.appendChild(httpGroup)

  const sftpGroup = el('div', 'prefs-export-group')
  const targetInput = el('input')
  targetInput.type = 'text'
  targetInput.placeholder = 'user@host:/path'
  sftpGroup.append(el('label', 'flow-field-label', 'Target'), targetInput)
  const toolSelect = el('select')
  for (const v of ['scp', 'rsync']) {
    const opt = el('option', null, v)
    opt.value = v
    toolSelect.appendChild(opt)
  }
  sftpGroup.append(el('label', 'flow-field-label', 'Tool'), toolSelect)
  m.body.appendChild(sftpGroup)

  const paintKind = () => {
    httpGroup.classList.toggle('hidden', kindSelect.value !== 'http')
    sftpGroup.classList.toggle('hidden', kindSelect.value !== 'sftp')
  }
  kindSelect.addEventListener('change', paintKind)
  paintKind()

  m.button('Continue', async () => {
    const kind = kindSelect.value
    const label = labelInput.value.trim()
    // Whichever of URL/Target belongs to the picked kind — kept as one
    // value through validation and the restated confirmation text, then
    // routed back into the right named field only in the consent() call
    // below, where url and target are genuinely two different columns.
    const primary = kind === 'http' ? urlInput.value.trim() : targetInput.value.trim()
    if (!label || !primary) {
      m.err.textContent = `label and ${kind === 'http' ? 'URL' : 'target'} are required`
      return
    }
    m.close()
    const restated = kind === 'http' ? `${methodSelect.value} ${primary}` : `${toolSelect.value} ${primary}`
    if (!(await confirmModal('Add export destination', `${label} — ${restated}`, 'Add'))) return
    try {
      await tome.exportDest.consent({
        kind,
        label,
        url: kind === 'http' ? primary : undefined,
        method: kind === 'http' ? methodSelect.value : undefined,
        authBearer: kind === 'http' ? authInput.value.trim() || undefined : undefined,
        target: kind === 'sftp' ? primary : undefined,
        tool: kind === 'sftp' ? toolSelect.value : undefined,
      })
      toast('export destination added', 'ok')
    } catch (err) {
      toast(err.message)
    }
  })
  m.button('Cancel', () => m.close(), 'ghost')
}

// ---- schedules ----
// The in-app scheduler (flow.js's Schedule… button, schedule.rs): every row
// here is a flow-schedules.json record main already hashed at schedules_set
// time. Main ticks every 30s, always gapped, and re-verifies the hash
// on every tick — a mismatch suspends the schedule rather than run content
// nobody reviewed, which is the state "Re-consent" below clears by calling
// schedules_set again with the schedule's own current fields (there is no
// separate reconsent command — see ipc/schedules.rs's own doc comment).
export async function buildSchedulesSection() {
  const section = el('section', 'prefs-section')
  section.append(el('h4', '', 'Schedules'))
  section.append(el('div', 'prefs-hint', 'flows scheduled from the Flow panel — always contained, all times UTC'))
  const list = el('div', 'prefs-agents')
  section.appendChild(list)

  const describeWhen = (when) =>
    when.kind === 'interval'
      ? `every ${when.minutes} minute${when.minutes === 1 ? '' : 's'}`
      : `daily at ${String(when.hour).padStart(2, '0')}:${String(when.minute).padStart(2, '0')} UTC`

  const renderRows = async () => {
    list.innerHTML = ''
    let schedules = []
    try {
      schedules = await tome.schedules.list()
    } catch (err) {
      list.appendChild(el('div', 'prefs-hint', err.message))
      return
    }
    if (!schedules.length) {
      const empty = el('div', 'prefs-hint', 'No schedules yet — use Schedule… on a flow.')
      empty.style.padding = '4px 0'
      list.appendChild(empty)
    }
    for (const s of schedules) {
      const r = el('div', 'prefs-row')
      const text = el('div', 'prefs-text')
      text.append(el('span', 'prefs-label', s.flowPath.split('/').pop()))
      const suspendedNote = s.suspended ? ` · suspended: ${s.suspended}` : ''
      text.append(el('span', 'prefs-hint', `${describeWhen(s.when)}${s.enabled ? '' : ' · disabled'}${suspendedNote}`))

      const controls = el('div', 'prefs-inline')
      if (s.suspended) {
        // The flow file changed since this schedule's own last consent — a
        // fresh schedules_set call re-reads and re-hashes it now, clearing
        // the suspension only if that succeeds.
        const reconsent = el('button', 'ag-btn ghost', 'Re-consent')
        reconsent.type = 'button'
        reconsent.addEventListener('click', async () => {
          try {
            await tome.schedules.set({ id: s.id, flowPath: s.flowPath, when: s.when, enabled: s.enabled })
            await renderRows()
            toast('schedule re-consented', 'ok')
          } catch (err) {
            toast(err.message)
          }
        })
        controls.appendChild(reconsent)
      } else {
        const sw = el('button', 'prefs-switch' + (s.enabled ? ' on' : ''))
        sw.type = 'button'
        sw.setAttribute('role', 'switch')
        sw.setAttribute('aria-checked', String(s.enabled))
        sw.append(el('span', 'prefs-knob'))
        sw.addEventListener('click', async () => {
          try {
            await tome.schedules.set({ id: s.id, flowPath: s.flowPath, when: s.when, enabled: !s.enabled })
            await renderRows()
          } catch (err) {
            toast(err.message)
          }
        })
        controls.appendChild(sw)
      }
      const remove = el('button', 'ag-btn ghost', 'Delete')
      remove.type = 'button'
      remove.addEventListener('click', async () => {
        await tome.schedules.delete(s.id)
        await renderRows()
        toast('schedule deleted', 'ok')
      })
      controls.appendChild(remove)

      r.append(text, controls)
      list.appendChild(r)
    }
  }
  await renderRows()
  return section
}

// ---- remote sources ----
// Consent-gated ssh destinations panels/runs.js's "Remote" section reads
// from (remote_runs/remote_run_detail, src-tauri/src/remote.rs). Every row
// here is a remote-sources.json entry main already hashed at
// remote_consent time — this section only ever lists what main already
// verified and only ever adds through that same consent path, mirroring
// buildExportSection immediately above almost exactly (see that function's
// doc comment for the shape this one repeats).
export async function buildRemoteSourcesSection(closePreferences) {
  const section = el('section', 'prefs-section')
  section.append(el('h4', '', 'Remote sources'))
  section.append(
    el('div', 'prefs-hint', 'ssh-reachable machines whose flow runs show up under Runs → Remote — read-only')
  )
  const list = el('div', 'prefs-agents')
  section.appendChild(list)

  const renderRows = async () => {
    list.innerHTML = ''
    let sources = []
    try {
      sources = await tome.remote.sources()
    } catch (err) {
      list.appendChild(el('div', 'prefs-hint', err.message))
      return
    }
    if (!sources.length) {
      const empty = el('div', 'prefs-hint', 'No remote sources yet — add one below.')
      empty.style.padding = '4px 0'
      list.appendChild(empty)
    }
    for (const s of sources) {
      const r = el('div', 'prefs-row')
      const text = el('div', 'prefs-text')
      text.append(el('span', 'prefs-label', s.label))
      text.append(el('span', 'prefs-hint', `${s.host} · ${s.repoPath}`))
      const remove = el('button', 'ag-btn ghost', 'Remove')
      remove.type = 'button'
      remove.addEventListener('click', async () => {
        await tome.remote.revoke(s.id)
        await renderRows()
        toast(`removed ${s.label}`, 'ok')
      })
      r.append(text, remove)
      list.appendChild(r)
    }
  }
  await renderRows()

  const add = el('button', 'ag-btn ghost', 'Add remote source…')
  add.type = 'button'
  add.addEventListener('click', () => {
    // modalShell keeps exactly one overlay at a time — a nested form has to
    // take Preferences' place, same as "Add destination…" above.
    closePreferences?.()
    openAddRemoteSourceModal()
  })
  section.appendChild(add)
  return section
}

// Label/Host/Repository-path form on modalShell — the same field(label,
// control) idiom openAddDestinationModal (above) uses. Submitting restates
// exactly what is consented to in a separate confirmModal before ever
// calling remote_consent: main never re-verifies host/repoPath against
// anything external (unlike the repo-allowlist flow's file hash — see
// remote.rs's own doc comment, "self-referential, same as
// export::Destination"), so this restatement is the only guard against a
// typo'd host or path.
function openAddRemoteSourceModal() {
  const m = modalShell('Add remote source')
  const field = (label, control) => {
    m.body.appendChild(el('label', 'flow-field-label', label))
    m.body.appendChild(control)
    return control
  }

  const labelInput = field('Label', el('input'))
  labelInput.type = 'text'
  labelInput.placeholder = 'e.g. Build server'

  const hostInput = field('Host', el('input'))
  hostInput.type = 'text'
  hostInput.placeholder = 'ssh alias or user@host'

  const repoInput = field('Repository path', el('input'))
  repoInput.type = 'text'
  repoInput.placeholder = '/abs/path/on/that/host'

  m.button('Continue', async () => {
    const label = labelInput.value.trim()
    const host = hostInput.value.trim()
    const repoPath = repoInput.value.trim()
    if (!label || !host || !repoPath) {
      m.err.textContent = 'label, host, and repository path are required'
      return
    }
    m.close()
    if (!(await confirmModal('Add remote source', `${label} — ${host}:${repoPath}`, 'Add'))) return
    try {
      await tome.remote.consent({ label, host, repoPath })
      toast('remote source added', 'ok')
    } catch (err) {
      toast(err.message)
    }
  })
  m.button('Cancel', () => m.close(), 'ghost')
}

// Label on the left, control on the right.
function row(parent, label, ctrl, hint) {
  const r = el('div', 'prefs-row')
  const text = el('div', 'prefs-text')
  text.append(el('span', 'prefs-label', label))
  if (hint) text.append(el('span', 'prefs-hint', hint))
  r.append(text, ctrl)
  parent.appendChild(r)
  return r
}

// Single-line text input that persists a store key on change; clearing the
// field writes null (no store:delete — null reads as unset).
function textRow(parent, label, hint, key, initial) {
  const input = el('input', 'prefs-input')
  input.type = 'text'
  input.spellcheck = false
  input.value = initial || ''
  input.addEventListener('change', () => {
    const v = input.value.trim()
    tome.store.set(key, v || null)
  })
  row(parent, label, input, hint)
  return input
}

// Switch-style toggle: a button with role=switch whose .on state mirrors the
// menu item's .active state. Reads/writes the exact store keys and prefs
// mutations from menus.js populateAddMenu.
function toggleRow(parent, label, hint, get, set) {
  const sw = el('button', 'prefs-switch')
  sw.type = 'button'
  sw.setAttribute('role', 'switch')
  const paint = () => {
    sw.classList.toggle('on', get())
    sw.setAttribute('aria-checked', String(get()))
  }
  sw.append(el('span', 'prefs-knob'))
  sw.addEventListener('click', () => {
    set(!get())
    paint()
  })
  row(parent, label, sw, hint)
  paint()
}

// ---- the Settings shell (slice 3a) ----
// modalShell keeps exactly one overlay at a time (its own doc comment), so
// the four flows that need a second modal — "Add destination…", "Add
// remote source…", "Enroll authenticator (2FA)…", "Replay setup wizard…" —
// still close Settings first. Each now stashes the group it should reopen
// at in pendingReopen just before closing; Settings' own onClose arms
// watchNestedOverlay, which watches the overlay through the flow's own
// chain (a form and its confirm modal replace each other synchronously;
// only showOnboarding mounts late, after an auth probe) and reopens
// Settings there when the last overlay unmounts — however the flow ends,
// cancel included.
let pendingReopen = null
const NESTED_MOUNT_TIMEOUT_MS = 10000

// A MutationObserver rather than a poll: browsers throttle timers in
// background/occluded windows, and a throttled interval can miss a
// short-lived nested modal entirely (never seeing it mount means never
// reopening). Mutation callbacks are microtasks — they fire exactly, in
// order, however the modal chain mounts and unmounts.
function watchNestedOverlay(section) {
  let sawOverlay = false
  const bail = setTimeout(() => {
    // The flow never mounted an overlay (showOnboarding's locked-workspace
    // path toasts instead) — stop watching, reopen nothing.
    if (!sawOverlay) {
      observer.disconnect()
      pendingReopen = null
    }
  }, NESTED_MOUNT_TIMEOUT_MS)
  const done = () => {
    observer.disconnect()
    clearTimeout(bail)
    pendingReopen = null
    // Deferred past the CURRENT keydown dispatch: keys.js's global Escape
    // handler runs later in the same dispatch and raw-removes whatever
    // #ag-overlay it finds — a synchronous reopen would mount straight
    // into that dispatch and be killed instantly (observed in practice).
    setTimeout(() => preferencesModal({ section }), 0)
  }
  const check = () => {
    const up = !!document.getElementById('ag-overlay')
    if (up) {
      sawOverlay = true
      return
    }
    if (sawOverlay) done()
  }
  const observer = new MutationObserver(check)
  observer.observe(document.body, { childList: true })
  // The launch site's own overlay is already gone by the time onClose arms
  // this (modalShell removes it before calling onClose), so the first sync
  // check correctly sees "nothing yet" — a nested flow that mounted AND
  // closed before we started watching still counts via its own mutations.
  check()
}

// Index text for one DOM node, excluding .prefs-row and h4 subtrees: the
// row/hint text plus every control's placeholder, value, and select
// options — so provider labels and model ids are findable even though
// they live in button text and input values, not text nodes.
function nodeSearchText(node) {
  let s = ''
  const walk = (n) => {
    for (const c of n.childNodes) {
      if (c.nodeType === 3) s += ' ' + c.textContent // Node.TEXT_NODE
      else if (c.nodeType === 1 && c.tagName !== 'H4' && !c.classList.contains('prefs-row')) walk(c)
    }
  }
  walk(node)
  for (const c of node.querySelectorAll('input, select')) {
    s += ' ' + (c.placeholder || '') + ' ' + (c.value || '')
    if (c.tagName === 'SELECT') for (const o of c.options) s += ' ' + o.textContent
  }
  return s
}

export async function preferencesModal({ section } = {}) {
  // A stale stash (a nested flow that never mounted) must not arm a
  // watcher on this instance's close.
  pendingReopen = null

  // The search box and its Esc-clear handler must exist BEFORE modalShell
  // registers its own document-level Esc-close: capture-phase keydown
  // listeners on the document run in registration order, and Esc inside
  // the search box means "clear the filter", not "close Settings" — so
  // this handler registers first and vetos the close with
  // stopImmediatePropagation (which also keeps keys.js's global Escape
  // handler from ever seeing the keypress).
  let activeQuery = ''
  const search = el('input', 'prefs-search')
  search.type = 'text'
  search.placeholder = 'Search settings…'
  search.setAttribute('aria-label', 'Search settings')
  search.spellcheck = false
  let clearSearch = () => {}
  const escSearch = (e) => {
    if (e.key === 'Escape' && document.activeElement === search && activeQuery) {
      e.stopImmediatePropagation()
      clearSearch()
    }
  }
  document.addEventListener('keydown', escSearch, true)

  const m = modalShell('Settings', () => {
    document.removeEventListener('keydown', escSearch, true)
    if (pendingReopen != null) watchNestedOverlay(pendingReopen)
  })
  m.err.remove() // no error line — prefs report via toasts
  m.body.parentElement.classList.add('prefs-box')

  // ---------- shell ----------
  // Left rail (search + seven groups + the setup-wizard footer), right
  // scrolling pane. Sections keep the .prefs-section/.prefs-row
  // vocabulary; they just render inside the pane now.
  const shell = el('div', 'prefs-shell')
  const nav = el('nav', 'prefs-nav')
  nav.setAttribute('aria-label', 'Settings groups')
  const pane = el('div', 'prefs-pane')
  shell.append(nav, pane)
  m.body.appendChild(shell)

  // ---------- live search ----------
  search.addEventListener('input', () => applyFilter(search.value))
  clearSearch = () => {
    search.value = ''
    applyFilter('')
  }
  const matchCount = el('div', 'prefs-match-count')
  matchCount.setAttribute('aria-live', 'polite')
  matchCount.hidden = true

  // The index is re-read from the pane on every pass: async sections fill
  // in after mount and list sections re-render their own rows, and a
  // fresh walk of a few dozen rows costs nothing. Non-row content (the
  // custom-provider form, hints like the login-shell key line) indexes as
  // one extra per-section entry so it stays findable.
  const applyFilter = (query) => {
    activeQuery = query
    const empty = normalize(query) === ''
    const index = new Map() // sectionId → { el, rows: [{el, text}] }
    const entries = [] // row + non-row entries
    const headings = [] // section titles, kept apart so a title hit shows the whole section
    for (const sec of pane.querySelectorAll('[data-section]')) {
      const id = sec.dataset.section
      const groupId = sectionToGroup(id)
      const rows = [...sec.querySelectorAll('.prefs-row')].map((r) => ({ el: r, text: nodeSearchText(r) }))
      index.set(id, { el: sec, rows })
      headings.push({ groupId, sectionId: id, text: sec.querySelector('h4')?.textContent || '' })
      entries.push({ groupId, sectionId: id, text: nodeSearchText(sec) })
      for (const r of rows) entries.push({ groupId, sectionId: id, text: r.text })
    }
    const hits = filterRows(query, entries)
    const headHits = filterRows(query, headings)
    for (const [id, s] of index) {
      s.el.classList.toggle('prefs-section-hidden', !empty && !hits.sections.has(id))
      const whole = empty || headHits.sections.has(id)
      for (const r of s.rows) {
        const rowHit = empty || filterRows(query, [{ groupId: sectionToGroup(id), sectionId: id, text: r.text }]).sections.has(id)
        r.el.classList.toggle('prefs-row-hidden', !(rowHit || whole))
      }
    }
    for (const b of groupBtns)
      b.classList.toggle('prefs-nav-dim', !empty && !hits.groups.has(b.dataset.group))
    if (empty) {
      matchCount.hidden = true
    } else {
      // scroll sync is suspended while filtering — drop the stale marker
      // rather than leave it pointing at a group the user can't see
      for (const b of groupBtns) b.removeAttribute('aria-current')
      matchCount.hidden = false
      matchCount.textContent = hits.count ? `${hits.count} of ${index.size} sections` : 'no matches'
    }
    syncNav()
  }

  // ---------- rail ----------
  nav.appendChild(search)
  nav.appendChild(matchCount)
  const groupBtns = []
  for (const g of GROUPS) {
    const b = el('button', 'prefs-nav-item', g.label)
    b.type = 'button'
    b.dataset.group = g.id
    b.addEventListener('click', () => scrollToGroup(g.id))
    nav.appendChild(b)
    groupBtns.push(b)
  }
  nav.appendChild(el('div', 'prefs-nav-divider'))
  const footer = el('div', 'prefs-nav-footer')
  const replay = el('button', 'prefs-nav-item prefs-nav-action', 'Replay setup wizard…')
  replay.type = 'button'
  replay.title = 'the first-run tour — agents, assistant, voice, security'
  replay.addEventListener('click', () => {
    pendingReopen = 'general'
    m.close()
    showOnboarding()
  })
  footer.appendChild(replay)
  nav.appendChild(footer)

  // '/' anywhere in the modal (outside a field) jumps to the search box.
  m.body.parentElement.addEventListener('keydown', (e) => {
    if (e.key !== '/') return
    const t = e.target
    if (t && (t.tagName === 'INPUT' || t.tagName === 'SELECT' || t.tagName === 'TEXTAREA')) return
    e.preventDefault()
    search.focus()
    search.select()
  })

  // ---------- nav ↔ pane scroll sync ----------
  // The rail button for the group most in view carries aria-current;
  // suspended while a search query is active (sections jump around under
  // a filter). rAF-throttled: scroll events fire per frame.
  let navRaf = 0
  const syncNav = () => {
    navRaf = 0
    if (activeQuery !== '') return
    const top = pane.getBoundingClientRect().top
    let current = null
    for (const sec of pane.querySelectorAll('[data-section]')) {
      if (sec.classList.contains('prefs-section-hidden')) continue
      if (sec.getBoundingClientRect().top - top <= 64) current = sec.dataset.section
      else break
    }
    // At the bottom of the pane the last sections can never cross the top
    // line — a jump to them must still highlight their group, not whatever
    // group happens to sit at the top when the scroll runs out.
    if (pane.scrollTop + pane.clientHeight >= pane.scrollHeight - 2) {
      const visible = [...pane.querySelectorAll('[data-section]')].filter(
        (s) => !s.classList.contains('prefs-section-hidden')
      )
      if (visible.length) current = visible[visible.length - 1].dataset.section
    }
    const g = current ? sectionToGroup(current) : GROUPS[0].id
    for (const b of groupBtns) {
      if (b.dataset.group === g) b.setAttribute('aria-current', 'true')
      else b.removeAttribute('aria-current')
    }
  }
  pane.addEventListener(
    'scroll',
    () => {
      if (!navRaf) navRaf = requestAnimationFrame(syncNav)
    },
    { passive: true }
  )

  const scrollToGroup = (groupId) => {
    for (const sec of pane.querySelectorAll('[data-section]')) {
      if (sectionToGroup(sec.dataset.section) !== groupId) continue
      if (sec.classList.contains('prefs-section-hidden')) continue // dimmed group under a filter: first surviving section
      sec.scrollIntoView({ behavior: 'smooth', block: 'start' })
      return
    }
  }

  // ---------- deep links ----------
  // preferencesModal({ section }) takes a section id or a group id —
  // infrastructure for later slices (chat header, egress-gap jump, the
  // Voice menu item); it scrolls to the target and flashes it, again once
  // the target's async content has landed.
  const resolveDeepLink = (id) => {
    if (!id) return null
    if (sectionToGroup(id)) return id // a section id
    const g = GROUPS.find((x) => x.id === id)
    return g ? g.sections[0] : null // a group id → its first section
  }
  const flash = (node) => {
    node.classList.add('prefs-highlight')
    setTimeout(() => node.classList.remove('prefs-highlight'), 1500)
  }
  const deepTarget = resolveDeepLink(section)
  let deepLink = deepTarget // cleared once the target's real content lands

  // ---------- sections ----------
  // Sync groups paint with the modal; async groups mount a placeholder in
  // their pane slot now and fill in as their probes land — every probe is
  // in flight before the first paint, so opening Settings never waits on
  // chat:providers' login-shell spawn (or any other IPC).
  const mount = (sec, id) => {
    sec.dataset.section = id
    pane.appendChild(sec)
    return sec
  }
  const startAsync = (sectionId, heading, build) => {
    const placeholder = el('section', 'prefs-section')
    placeholder.dataset.section = sectionId
    placeholder.append(el('h4', '', heading))
    const hint = el('div', 'prefs-hint', 'Loading…')
    placeholder.appendChild(hint)
    pane.appendChild(placeholder)
    Promise.resolve()
      .then(build) // one microtask later: the modal paints first, with placeholders
      .then((real) => {
        if (!pane.isConnected) return // modal closed while the probe was out
        real.dataset.section = sectionId
        placeholder.replaceWith(real)
        applyFilter(activeQuery) // a late arrival respects the live query
        syncNav()
        if (deepLink === sectionId) {
          deepLink = null
          real.scrollIntoView({ block: 'start' })
          flash(real)
        }
      })
      .catch((err) => {
        if (!pane.isConnected) return
        // Builders catch their own expected failures; this is the net.
        hint.textContent = err?.message || 'Could not load this section.'
        applyFilter(activeQuery)
      })
  }
  // buildExportSection / buildRemoteSourcesSection take a "close
  // Preferences" callback their add-buttons call right before mounting
  // their own modal — this variant also stashes where to come back to.
  const closeReopeningAt = (groupId) => () => {
    pendingReopen = groupId
    m.close()
  }

  // ===== general =====

  // ---------- appearance ----------
  const buildAppearance = () => {
    const section = el('section', 'prefs-section')
    section.append(el('h4', '', 'Appearance'))
    const seg = el('div', 'prefs-seg')
    seg.setAttribute('role', 'radiogroup')
    seg.setAttribute('aria-label', 'Theme')
    for (const pref of THEME_ORDER) {
      const b = el('button', '', `${THEME_GLYPH[pref]} ${THEME_LABEL[pref]}`)
      b.type = 'button'
      b.setAttribute('role', 'radio')
      b.setAttribute('aria-checked', String(themeState.pref === pref))
      b.classList.toggle('on', themeState.pref === pref)
      b.addEventListener('click', () => {
        setTheme(pref) // persists 'theme' and re-skins live
        for (const s of seg.children) {
          const on = s === b
          s.classList.toggle('on', on)
          s.setAttribute('aria-checked', String(on))
        }
      })
      seg.appendChild(b)
    }
    row(section, 'Theme', seg)
    return section
  }

  // ---------- terminal ----------
  const buildTerminal = () => {
    const section = el('section', 'prefs-section')
    section.append(el('h4', '', 'Terminal'))
    // Paint with the default; the stored size is a fast store read that
    // corrects the row when it lands — the modal must not wait on IPC.
    let fontSize = TERM_FONT.default
    const stepper = el('div', 'prefs-stepper')
    const value = el('span', 'prefs-value', String(fontSize))
    const paint = () => {
      value.textContent = String(fontSize)
      minus.disabled = fontSize <= TERM_FONT.min
      plus.disabled = fontSize >= TERM_FONT.max
    }
    const apply = (next) => {
      fontSize = Math.min(TERM_FONT.max, Math.max(TERM_FONT.min, next))
      // setTermFontSize applies to every live terminal and persists
      // 'term-font-size', keeping Preferences and ⌘=/⌘-/⌘0 in sync
      setTermFontSize(fontSize)
      paint()
    }
    const minus = el('button', '', '−')
    minus.type = 'button'
    minus.setAttribute('aria-label', 'Decrease terminal font size')
    minus.addEventListener('click', () => apply(fontSize - 1))
    const plus = el('button', '', '+')
    plus.type = 'button'
    plus.setAttribute('aria-label', 'Increase terminal font size')
    plus.addEventListener('click', () => apply(fontSize + 1))
    stepper.append(minus, value, plus)
    row(section, 'Font size', stepper, `${TERM_FONT.min}–${TERM_FONT.max} · ⌘= / ⌘- / ⌘0`)
    paint()
    tome.store
      .get('term-font-size')
      .then((size) => {
        if (typeof size === 'number' && size >= TERM_FONT.min && size <= TERM_FONT.max) {
          fontSize = size
          paint()
        }
      })
      .catch(() => {})
    return section
  }

  // ---------- editor ----------
  const buildEditor = () => {
    const section = el('section', 'prefs-section')
    section.append(el('h4', '', 'Editor'))
    const tabStep = el('div', 'prefs-stepper')
    const tabValue = el('span', 'prefs-value', String(editorPrefs.tabSize))
    const setTab = (n) => {
      const size = Math.min(8, Math.max(1, n))
      setEditorPrefs({ tabSize: size })
      tabValue.textContent = String(size)
      tabMinus.disabled = size <= 1
      tabPlus.disabled = size >= 8
    }
    const tabMinus = el('button', '', '−')
    tabMinus.type = 'button'
    tabMinus.setAttribute('aria-label', 'Decrease indent size')
    tabMinus.addEventListener('click', () => setTab(editorPrefs.tabSize - 1))
    const tabPlus = el('button', '', '+')
    tabPlus.type = 'button'
    tabPlus.setAttribute('aria-label', 'Increase indent size')
    tabPlus.addEventListener('click', () => setTab(editorPrefs.tabSize + 1))
    tabStep.append(tabMinus, tabValue, tabPlus)
    row(section, 'Indent size', tabStep, 'spaces per Tab · 1–8')
    tabMinus.disabled = editorPrefs.tabSize <= 1
    tabPlus.disabled = editorPrefs.tabSize >= 8
    toggleRow(
      section,
      'Wrap long lines',
      'soft-wrap instead of scrolling sideways',
      () => editorPrefs.wrap,
      (v) => setEditorPrefs({ wrap: v })
    )
    toggleRow(
      section,
      'Trim trailing whitespace on save',
      'applied to the buffer, so the pane stays clean',
      () => editorPrefs.trimOnSave,
      (v) => setEditorPrefs({ trimOnSave: v })
    )
    toggleRow(
      section,
      'Format on save',
      'Prettier, using the project’s own config',
      () => editorPrefs.formatOnSave,
      (v) => setEditorPrefs({ formatOnSave: v })
    )
    toggleRow(
      section,
      'Autosave',
      'save a moment after you stop typing',
      () => editorPrefs.autosave,
      (v) => setEditorPrefs({ autosave: v })
    )
    return section
  }

  // ---------- sidebar ----------
  const buildSidebar = () => {
    const section = el('section', 'prefs-section')
    section.append(el('h4', '', 'Sidebar'))
    const tree = document.getElementById('tree')
    const reset = el('button', 'ag-btn ghost', 'Reset width')
    reset.type = 'button'
    const width = el('span', 'prefs-value')
    const paintWidth = () => {
      // Read on demand (click + open), not during render — a layout read in
      // the build path forces sync reflow of the whole app under the modal.
      const current = Math.round(tree.getBoundingClientRect().width) || SIDEBAR_DEFAULT
      width.textContent = `${current} px`
    }
    reset.addEventListener('click', () => {
      tree.style.width = ''
      tome.store.set('sidebar-width', null) // no store:delete — null reads as unset
      paintWidth()
      toast('Sidebar width reset', 'ok')
    })
    const widthBox = el('div', 'prefs-inline')
    widthBox.append(width, reset)
    row(section, 'Width', widthBox, `drag the divider · default ${SIDEBAR_DEFAULT} px`)
    paintWidth()
    return section
  }

  // ===== assistant =====

  // ---------- assistant ----------
  // Provider choice + model override for the assistant pane. Keys are NOT
  // stored: they come from the login shell (main's ensureLoginEnv), so this
  // section only shows whether each key was found — never the key itself.
  // Async: chat:providers awaits a login-shell spawn in main, so the
  // section fills in whenever that probe returns.
  const buildAssistant = async () => {
    const section = el('section', 'prefs-section')
    section.append(el('h4', '', 'Assistant'))
    const chatInfo = await tome.chat.providers().catch(() => null)
    if (chatInfo) {
      const pseg = el('div', 'prefs-seg')
      pseg.setAttribute('role', 'radiogroup')
      pseg.setAttribute('aria-label', 'Assistant provider')
      const modelRow = { input: null } // filled below; provider switch repopulates it
      for (const p of chatInfo.providers) {
        // ● key found in the login shell · ○ missing — set it and restart
        const b = el('button', '', `${p.keySet ? '●' : '○'} ${p.label}`)
        b.type = 'button'
        b.setAttribute('role', 'radio')
        b.title = p.keyEnv
          ? p.keySet
            ? `${p.keyEnv} found in your login shell`
            : `${p.keyEnv} not found in your login shell`
          : p.keySet
            ? 'custom provider configured in Settings'
            : 'custom provider not configured — set it below'
        b.setAttribute('aria-checked', String(chatInfo.active === p.id))
        b.classList.toggle('on', chatInfo.active === p.id)
        b.addEventListener('click', () => {
          tome.store.set('chat-provider', p.id)
          for (const s of pseg.children) {
            const on = s === b
            s.classList.toggle('on', on)
            s.setAttribute('aria-checked', String(on))
          }
          // Repopulate the model field with the newly picked provider's
          // default; the stored override only makes sense per provider.
          if (modelRow.input) {
            modelRow.input.value = p.model
            tome.store.set('chat-model', null)
          }
        })
        pseg.appendChild(b)
      }
      row(section, 'Provider', pseg, '● key found in your login shell · ○ missing')
      const activeEntry = chatInfo.providers.find((p) => p.id === chatInfo.active)
      const storedModel = await tome.store.get('chat-model')
      modelRow.input = textRow(
        section,
        'Model',
        'blank = provider default',
        'chat-model',
        typeof storedModel === 'string' && storedModel ? storedModel : activeEntry?.model
      )
      const keysHint = el(
        'div',
        'prefs-hint',
        'keys come from your login shell — set MOONSHOT_API_KEY / ZHIPU_API_KEY / ANTHROPIC_API_KEY / DEEPSEEK_API_KEY and restart'
      )
      section.appendChild(keysHint)
    } else {
      section.appendChild(el('div', 'prefs-hint', 'Provider list unavailable.'))
    }
    return section
  }

  // ---------- custom provider ("any provider") ----------
  // An OpenAI- or Anthropic-compatible endpoint the user supplies. Unlike the
  // built-ins (whose key comes from a login-shell env var), the key is stored
  // in the 0600 JSON store so it can be pasted here — it never reaches the
  // browser layer beyond this form.
  const buildCustomProvider = async () => {
    const section = el('section', 'prefs-section')
    section.append(el('h4', '', 'Custom provider'))
    const custom = (await tome.store.get('custom-provider')) || {}
    const cpInput = (key, placeholder, type = 'text') => {
      const i = el('input', 'prefs-input')
      i.type = type
      i.placeholder = placeholder
      i.spellcheck = false
      i.value = custom[key] || ''
      i.setAttribute('aria-label', placeholder)
      return i
    }
    const cpLabel = cpInput('label', 'label — e.g. My endpoint')
    const cpBase = cpInput('baseUrl', 'base URL — e.g. https://api.deepseek.com/v1')
    const cpModel = cpInput('model', 'model id — e.g. deepseek-v4-pro')
    const cpKey = cpInput('key', 'API key', 'password')
    const wireSeg = el('div', 'prefs-seg')
    wireSeg.setAttribute('role', 'radiogroup')
    wireSeg.setAttribute('aria-label', 'Custom provider wire')
    let wire = custom.wire === 'anthropic' ? 'anthropic' : 'openai'
    const wireOpenai = el('button', '', 'OpenAI')
    const wireAnth = el('button', '', 'Anthropic')
    for (const b of [wireOpenai, wireAnth]) {
      b.type = 'button'
      b.setAttribute('role', 'radio')
    }
    const paintWire = () => {
      wireOpenai.classList.toggle('on', wire === 'openai')
      wireAnth.classList.toggle('on', wire === 'anthropic')
      wireOpenai.setAttribute('aria-checked', String(wire === 'openai'))
      wireAnth.setAttribute('aria-checked', String(wire === 'anthropic'))
    }
    wireOpenai.addEventListener('click', () => {
      wire = 'openai'
      paintWire()
    })
    wireAnth.addEventListener('click', () => {
      wire = 'anthropic'
      paintWire()
    })
    wireSeg.append(wireOpenai, wireAnth)
    paintWire()
    const cpSave = el('button', 'ag-btn ghost', 'Save custom provider')
    cpSave.type = 'button'
    cpSave.addEventListener('click', async () => {
      const value = {
        label: cpLabel.value.trim(),
        baseUrl: cpBase.value.trim(),
        model: cpModel.value.trim(),
        key: cpKey.value.trim(),
        wire,
      }
      if (!value.label || !value.baseUrl || !value.model || !value.key) {
        return toast('fill in label, base URL, model, and key')
      }
      await tome.store.set('custom-provider', value)
      toast('custom provider saved — select it under Provider', 'ok')
    })
    const cpClear = el('button', 'ag-btn ghost', 'Clear')
    cpClear.type = 'button'
    cpClear.addEventListener('click', async () => {
      await tome.store.set('custom-provider', null)
      cpLabel.value = cpBase.value = cpModel.value = cpKey.value = ''
      toast('custom provider cleared', 'ok')
    })
    section.append(cpLabel, cpBase, cpModel, cpKey, wireSeg, cpSave, cpClear)
    section.append(
      el('div', 'prefs-hint', 'the key is stored locally in the 0600 store — never shown to a browser or logged')
    )
    return section
  }

  // ===== agents ===== (buildAgentsSection, exported above — WS-E
  // onboarding mounts it on its own surface; here it is an async group)

  // ===== security =====

  // ---------- security ----------
  const buildSecurity = () => {
    const section = el('section', 'prefs-section')
    section.append(el('h4', '', 'Security'))
    toggleRow(
      section,
      'Spawn agents contained',
      null,
      () => prefs.egressDefault,
      (v) => {
        prefs.egressDefault = v
        tome.store.set('egress-default', v)
      }
    )
    toggleRow(
      section,
      'Assistant may run commands',
      null,
      () => prefs.conductorRun,
      (v) => {
        prefs.conductorRun = v
        tome.store.set('conductor-run', v)
        tome.conductor.allowRun(v)
      }
    )
    toggleRow(
      section,
      'Allow sandboxed Docker',
      'a filtered gateway, never the real daemon socket — default off',
      () => prefs.dockerGateway,
      (v) => {
        prefs.dockerGateway = v
        tome.store.set('docker-gateway', v)
      }
    )
    const enroll = el('button', 'ag-btn ghost', 'Enroll authenticator (2FA)…')
    enroll.type = 'button'
    enroll.addEventListener('click', () => {
      // One overlay at a time: totpModal takes Settings' place, and the
      // stash + watchNestedOverlay reopen it at this group afterwards.
      pendingReopen = 'security'
      m.close()
      totpModal()
    })
    row(section, 'Two-factor authentication', enroll, 'required to open a contained pane')
    return section
  }

  // ===== integrations ===== (export / schedules / remote — the three
  // module-level builders above, in that group's slot order)

  // ===== voice =====

  // ---------- voice ----------
  // Whisper availability + the launch warm-up opt-in the onboarding wizard's
  // Voice step writes — both surfaces use the same store key and the same
  // stt:status probe, so they can never disagree. The speech-engine select
  // below resolves to Apple's on-device recognizer or whisper.cpp through
  // the same resolution, so the hint can never claim an engine the probe
  // didn't pick.
  const buildVoice = async () => {
    const section = el('section', 'prefs-section')
    section.append(el('h4', '', 'Voice'))
    const sttStatus = el('div', 'prefs-hint', 'Checking local speech…')
    // Task 5: one-click model download, shown only when whisper is the resolved
    // engine and the model (not the binary) is what's missing — the binary
    // still needs `brew install whisper-cpp` first, which no in-app button can
    // do for the user.
    const downloadBtn = el('button', 'ag-btn ghost', 'Download speech model')
    downloadBtn.type = 'button'
    downloadBtn.hidden = true
    const statusRow = el('div', 'prefs-inline')
    statusRow.append(sttStatus, downloadBtn)
    section.appendChild(statusRow)
    const paintStatus = (s) => {
      if (s.engine === 'apple') {
        sttStatus.textContent = s.ready ? 'Apple on-device dictation — ready.' : s.why
        downloadBtn.hidden = true
      } else if (s.ready) {
        sttStatus.textContent = 'Local whisper transcription is ready.'
        downloadBtn.hidden = true
      } else if (!s.bin) {
        sttStatus.textContent = 'whisper-cli not found — install it (brew install whisper-cpp) and restart.'
        downloadBtn.hidden = true
      } else {
        sttStatus.textContent = 'Speech model not downloaded.'
        downloadBtn.hidden = false
      }
    }
    downloadBtn.addEventListener('click', async () => {
      downloadBtn.disabled = true
      downloadBtn.textContent = 'Downloading…'
      try {
        const res = await tome.stt.downloadModel()
        if (res?.error) {
          sttStatus.textContent = res.error
        } else {
          sttStatus.textContent = 'Speech model downloaded.'
          const s = await tome.stt.status().catch(() => null)
          if (s) paintStatus(s)
        }
      } catch (err) {
        sttStatus.textContent = err?.message || 'Download failed.'
      } finally {
        downloadBtn.disabled = false
        downloadBtn.textContent = 'Download speech model'
      }
    })
    // Both probes in flight at once — the section lands as one piece.
    const [warmup, engineInfo, sttReady] = await Promise.all([
      tome.store.get('voice-warmup').catch(() => null),
      tome.stt.engine().catch(() => null),
      tome.stt.status().catch(() => null),
    ])
    let voiceWarmup = !!warmup
    if (sttReady) paintStatus(sttReady)
    else sttStatus.textContent = 'Whisper status unavailable.'

    const engineSelect = el('select')
    for (const [value, label] of [
      ['auto', 'Auto'],
      ['apple', 'Apple on-device'],
      ['whisper', 'whisper.cpp'],
    ]) {
      const opt = el('option', null, label)
      opt.value = value
      engineSelect.appendChild(opt)
    }
    // Restore the select from stt:engine's normalized `preference` field, not
    // the raw store key — a never-set/cleared key normalizes to "auto" on both
    // sides, so this never has to guess what a missing key means.
    engineSelect.value = engineInfo?.preference || 'auto'
    engineSelect.addEventListener('change', () => {
      tome.store.set('stt-engine', engineSelect.value)
      tome.stt
        .status()
        .then(paintStatus)
        .catch(() => (sttStatus.textContent = 'Whisper status unavailable.'))
    })
    row(
      section,
      'Speech engine',
      engineSelect,
      'Apple uses on-device dictation; whisper.cpp needs the local CLI and model'
    )
    toggleRow(
      section,
      'Warm up whisper at launch',
      'loads the speech model in the background so the first dictation is instant',
      () => voiceWarmup,
      (v) => {
        voiceWarmup = v
        tome.store.set('voice-warmup', v)
      }
    )
    return section
  }

  // ===== mentor =====

  // ---------- mentor ----------
  const buildMentor = () => {
    const section = el('section', 'prefs-section')
    section.append(el('h4', '', 'Mentor'))
    toggleRow(
      section,
      'Verbose guide (default)',
      'new workspaces teach rather than just do',
      () => mentorState.verboseDefault,
      (v) => saveMentorSettings({ verboseDefault: v })
    )
    toggleRow(
      section,
      'Test before implementing',
      'the mentor writes a failing test and checks understanding first',
      () => mentorState.gate,
      (v) => saveMentorSettings({ gate: v })
    )
    toggleRow(
      section,
      'Gate before commit',
      null,
      () => mentorState.gatePoints.commit,
      (v) => saveMentorSettings({ gatePoints: { ...mentorState.gatePoints, commit: v } })
    )
    toggleRow(
      section,
      'Gate before push',
      null,
      () => mentorState.gatePoints.push,
      (v) => saveMentorSettings({ gatePoints: { ...mentorState.gatePoints, push: v } })
    )
    const thrInput = el('input', 'prefs-input')
    thrInput.type = 'number'
    thrInput.min = '0'
    thrInput.max = '100'
    thrInput.value = String(mentorState.threshold)
    thrInput.addEventListener('change', () => {
      const n = Math.max(0, Math.min(100, Number(thrInput.value) || 0))
      saveMentorSettings({ threshold: n })
      thrInput.value = String(n)
    })
    row(section, 'Pass threshold', thrInput, 'understanding score needed to pass a gate · 0–100')
    const mix = el('div', 'prefs-mix')
    const MIX = [
      ['multiple_choice', 'Multiple choice'],
      ['true_false', 'True / false'],
      ['short_answer', 'Short answer'],
      ['code', 'Code'],
    ]
    for (const [key, label] of MIX) {
      const item = el('span', 'prefs-mix-item')
      const sw = el('button', 'prefs-switch')
      sw.type = 'button'
      sw.setAttribute('role', 'switch')
      sw.append(el('span', 'prefs-knob'))
      const paint = () => {
        const on = mentorState.questionTypes.includes(key)
        sw.classList.toggle('on', on)
        sw.setAttribute('aria-checked', String(on))
      }
      sw.addEventListener('click', () => {
        const next = mentorState.questionTypes.includes(key)
          ? mentorState.questionTypes.filter((t) => t !== key)
          : [...mentorState.questionTypes, key]
        saveMentorSettings({ questionTypes: next })
        paint()
      })
      item.append(el('span', 'prefs-mix-label', label), sw)
      mix.appendChild(item)
      paint()
    }
    row(section, 'Question mix', mix, 'which kinds of question the gate may ask')
    const resetUq = el('button', 'ag-btn ghost', 'Reset understanding score')
    resetUq.type = 'button'
    resetUq.addEventListener('click', () => {
      if (!activeWorkspace()) return toast('no active workspace to reset')
      setUq(0)
      toast('understanding score reset', 'ok')
    })
    row(section, 'Understanding score', resetUq, `per workspace · currently ${uq()}`)
    return section
  }

  // ---------- mount, in group order ----------
  // general → assistant → agents → security → integrations → voice → mentor
  mount(buildAppearance(), 'appearance')
  mount(buildTerminal(), 'terminal')
  mount(buildEditor(), 'editor')
  mount(buildSidebar(), 'sidebar')
  startAsync('assistant', 'Assistant', buildAssistant)
  startAsync('custom-provider', 'Custom provider', buildCustomProvider)
  startAsync('agents', 'Agents', () => buildAgentsSection())
  mount(buildSecurity(), 'security')
  startAsync('export', 'Export destinations', () => buildExportSection(closeReopeningAt('integrations')))
  startAsync('schedules', 'Schedules', () => buildSchedulesSection())
  startAsync('remote', 'Remote sources', () => buildRemoteSourcesSection(closeReopeningAt('integrations')))
  startAsync('voice', 'Voice', buildVoice)
  mount(buildMentor(), 'mentor')

  applyFilter('') // index the sync sections, set the rail's initial state
  if (deepTarget)
    setTimeout(() => {
      // One tick after paint: placeholders already hold every slot, so the
      // scroll lands even when the target's own content is still loading.
      const t = pane.querySelector(`[data-section="${deepTarget}"]`)
      if (t && pane.isConnected) {
        t.scrollIntoView({ block: 'start' })
        flash(t)
      }
    }, 0)
}
