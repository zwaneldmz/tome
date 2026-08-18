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

export async function preferencesModal() {
  const m = modalShell('Settings')
  m.err.remove() // no error line — prefs report via toasts
  m.body.parentElement.classList.add('prefs-box')
  let voiceWarmup = !!(await tome.store.get('voice-warmup'))

  // ---------- appearance ----------
  const appearance = el('section', 'prefs-section')
  appearance.append(el('h4', '', 'Appearance'))
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
  row(appearance, 'Theme', seg)
  m.body.appendChild(appearance)

  // ---------- terminal ----------
  const terminal = el('section', 'prefs-section')
  terminal.append(el('h4', '', 'Terminal'))
  const size = await tome.store.get('term-font-size')
  let fontSize =
    typeof size === 'number' && size >= TERM_FONT.min && size <= TERM_FONT.max ? size : TERM_FONT.default
  const stepper = el('div', 'prefs-stepper')
  const value = el('span', 'prefs-value', String(fontSize))
  const apply = (next) => {
    fontSize = Math.min(TERM_FONT.max, Math.max(TERM_FONT.min, next))
    // setTermFontSize applies to every live terminal and persists
    // 'term-font-size', keeping Preferences and ⌘=/⌘-/⌘0 in sync
    setTermFontSize(fontSize)
    value.textContent = String(fontSize)
    minus.disabled = fontSize <= TERM_FONT.min
    plus.disabled = fontSize >= TERM_FONT.max
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
  row(terminal, 'Font size', stepper, `${TERM_FONT.min}–${TERM_FONT.max} · ⌘= / ⌘- / ⌘0`)
  minus.disabled = fontSize <= TERM_FONT.min
  plus.disabled = fontSize >= TERM_FONT.max
  m.body.appendChild(terminal)

  // ---------- editor ----------
  const editor = el('section', 'prefs-section')
  editor.append(el('h4', '', 'Editor'))
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
  row(editor, 'Indent size', tabStep, 'spaces per Tab · 1–8')
  tabMinus.disabled = editorPrefs.tabSize <= 1
  tabPlus.disabled = editorPrefs.tabSize >= 8
  toggleRow(
    editor,
    'Wrap long lines',
    'soft-wrap instead of scrolling sideways',
    () => editorPrefs.wrap,
    (v) => setEditorPrefs({ wrap: v })
  )
  toggleRow(
    editor,
    'Trim trailing whitespace on save',
    'applied to the buffer, so the pane stays clean',
    () => editorPrefs.trimOnSave,
    (v) => setEditorPrefs({ trimOnSave: v })
  )
  toggleRow(
    editor,
    'Format on save',
    'Prettier, using the project’s own config',
    () => editorPrefs.formatOnSave,
    (v) => setEditorPrefs({ formatOnSave: v })
  )
  toggleRow(
    editor,
    'Autosave',
    'save a moment after you stop typing',
    () => editorPrefs.autosave,
    (v) => setEditorPrefs({ autosave: v })
  )
  m.body.appendChild(editor)

  // ---------- assistant ----------
  // Provider choice + model override for the assistant pane. Keys are NOT
  // stored: they come from the login shell (main's ensureLoginEnv), so this
  // section only shows whether each key was found — never the key itself.
  const assistant = el('section', 'prefs-section')
  assistant.append(el('h4', '', 'Assistant'))
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
    row(assistant, 'Provider', pseg, '● key found in your login shell · ○ missing')
    const activeEntry = chatInfo.providers.find((p) => p.id === chatInfo.active)
    const storedModel = await tome.store.get('chat-model')
    modelRow.input = textRow(
      assistant,
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
    assistant.appendChild(keysHint)
  } else {
    assistant.appendChild(el('div', 'prefs-hint', 'Provider list unavailable.'))
  }
  m.body.appendChild(assistant)

  // ---------- custom provider ("any provider") ----------
  // An OpenAI- or Anthropic-compatible endpoint the user supplies. Unlike the
  // built-ins (whose key comes from a login-shell env var), the key is stored
  // in the 0600 JSON store so it can be pasted here — it never reaches the
  // browser layer beyond this form.
  const customSection = el('section', 'prefs-section')
  customSection.append(el('h4', '', 'Custom provider'))
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
  customSection.append(cpLabel, cpBase, cpModel, cpKey, wireSeg, cpSave, cpClear)
  customSection.append(
    el('div', 'prefs-hint', 'the key is stored locally in the 0600 store — never shown to a browser or logged')
  )
  m.body.appendChild(customSection)

  // ---------- security ----------
  const security = el('section', 'prefs-section')
  security.append(el('h4', '', 'Security'))
  toggleRow(
    security,
    'Spawn agents contained',
    null,
    () => prefs.egressDefault,
    (v) => {
      prefs.egressDefault = v
      tome.store.set('egress-default', v)
    }
  )
  toggleRow(
    security,
    'Assistant may run commands',
    null,
    () => prefs.conductorRun,
    (v) => {
      prefs.conductorRun = v
      tome.store.set('conductor-run', v)
      tome.conductor.allowRun(v)
    }
  )
  const enroll = el('button', 'ag-btn ghost', 'Enroll authenticator (2FA)…')
  enroll.type = 'button'
  enroll.addEventListener('click', () => {
    m.close()
    totpModal()
  })
  row(security, 'Two-factor authentication', enroll, 'required to open a contained pane')
  m.body.appendChild(security)

  // ---------- export destinations ----------
  m.body.appendChild(await buildExportSection(m.close))

  // ---------- schedules ----------
  m.body.appendChild(await buildSchedulesSection())

  // ---------- remote sources ----------
  m.body.appendChild(await buildRemoteSourcesSection(m.close))

  // ---------- voice ----------
  // Whisper availability + the launch warm-up opt-in the onboarding wizard's
  // Voice step writes — both surfaces use the same store key and the same
  // stt:status probe, so they can never disagree. The speech-engine select
  // below resolves to Apple's on-device recognizer or whisper.cpp through
  // the same resolution, so the hint can never claim an engine the probe
  // didn't pick.
  const voice = el('section', 'prefs-section')
  voice.append(el('h4', '', 'Voice'))
  const sttStatus = el('div', 'prefs-hint', 'Checking local speech…')
  voice.appendChild(sttStatus)
  const paintStatus = (s) => {
    if (s.engine === 'apple') {
      sttStatus.textContent = s.ready ? 'Apple on-device dictation — ready.' : s.why
    } else if (s.ready) {
      sttStatus.textContent = 'Local whisper transcription is ready.'
    } else if (!s.bin) {
      sttStatus.textContent = 'whisper-cli not found — install it (brew install whisper-cpp) and restart.'
    } else {
      sttStatus.textContent = 'Speech model missing — the push-to-talk error message carries the one-time download command.'
    }
  }
  tome.stt
    .status()
    .then(paintStatus)
    .catch(() => (sttStatus.textContent = 'Whisper status unavailable.'))

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
  const engineInfo = await tome.stt.engine().catch(() => null)
  engineSelect.value = engineInfo?.preference || 'auto'
  engineSelect.addEventListener('change', () => {
    tome.store.set('stt-engine', engineSelect.value)
    tome.stt
      .status()
      .then(paintStatus)
      .catch(() => (sttStatus.textContent = 'Whisper status unavailable.'))
  })
  row(
    voice,
    'Speech engine',
    engineSelect,
    'Apple uses on-device dictation; whisper.cpp needs the local CLI and model'
  )
  toggleRow(
    voice,
    'Warm up whisper at launch',
    'loads the speech model in the background so the first dictation is instant',
    () => voiceWarmup,
    (v) => {
      voiceWarmup = v
      tome.store.set('voice-warmup', v)
    }
  )
  m.body.appendChild(voice)

  // ---------- agents ----------
  m.body.appendChild(await buildAgentsSection())

  // ---------- sidebar ----------
  const sidebar = el('section', 'prefs-section')
  sidebar.append(el('h4', '', 'Sidebar'))
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
  row(sidebar, 'Width', widthBox, `drag the divider · default ${SIDEBAR_DEFAULT} px`)
  paintWidth()
  m.body.appendChild(sidebar)

  // ---------- mentor ----------
  const mentor = el('section', 'prefs-section')
  mentor.append(el('h4', '', 'Mentor'))
  toggleRow(
    mentor,
    'Verbose guide (default)',
    'new workspaces teach rather than just do',
    () => mentorState.verboseDefault,
    (v) => saveMentorSettings({ verboseDefault: v })
  )
  toggleRow(
    mentor,
    'Test before implementing',
    'the mentor writes a failing test and checks understanding first',
    () => mentorState.gate,
    (v) => saveMentorSettings({ gate: v })
  )
  toggleRow(
    mentor,
    'Gate before commit',
    null,
    () => mentorState.gatePoints.commit,
    (v) => saveMentorSettings({ gatePoints: { ...mentorState.gatePoints, commit: v } })
  )
  toggleRow(
    mentor,
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
  row(mentor, 'Pass threshold', thrInput, 'understanding score needed to pass a gate · 0–100')
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
  row(mentor, 'Question mix', mix, 'which kinds of question the gate may ask')
  const resetUq = el('button', 'ag-btn ghost', 'Reset understanding score')
  resetUq.type = 'button'
  resetUq.addEventListener('click', () => {
    if (!activeWorkspace()) return toast('no active workspace to reset')
    setUq(0)
    toast('understanding score reset', 'ok')
  })
  row(mentor, 'Understanding score', resetUq, `per workspace · currently ${uq()}`)
  m.body.appendChild(mentor)

  // ---------- onboarding ----------
  const onboarding = el('section', 'prefs-section')
  onboarding.append(el('h4', '', 'Onboarding'))
  const replay = el('button', 'ag-btn ghost', 'Replay setup wizard…')
  replay.type = 'button'
  replay.addEventListener('click', () => {
    m.close()
    showOnboarding()
  })
  row(onboarding, 'Setup wizard', replay, 'the first-run tour — agents, assistant, voice, security')
  m.body.appendChild(onboarding)
}
