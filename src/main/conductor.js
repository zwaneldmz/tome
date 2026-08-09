// Conductor: gives the assistant chat eyes and hands over the workspace.
// Tracks pty scrollback + the renderer's pane list, exposes a small tool set
// to the Claude chat loop, and never runs a command unless the user flipped
// the "assistant may run commands" toggle (allowRun) on.
import { stripAnsi, stripControlChars } from '../shared/terminal-text.js'
import { AGENTS, OPENABLE_KINDS_DESCRIPTION } from '../shared/pane-kinds.js'
import { readFlowTool, draftFlowTool } from './lib/flow-tools.js'

let ptys = null // Map shared with index.js
let send = () => {} // (channel, payload) -> renderer
let panes = [] // renderer's pane snapshot [{ id, title }]
let allowRun = false
let canOpenFile = () => false // main's workspace confinement check
let logEvent = () => {} // main's persistent event log (events.js)
let getRoots = () => [] // open workspace folders, [] until ws:sync lands

const meta = new Map() // ptyId -> { kind, cwd, airgap, exited }
const scrolls = new Map() // ptyId -> recent raw output
const SCROLL_CAP = 200_000

export function init(opts) {
  ptys = opts.ptys
  send = opts.send
  if (typeof opts.canOpenFile === 'function') canOpenFile = opts.canOpenFile
  if (typeof opts.logEvent === 'function') logEvent = opts.logEvent
  if (typeof opts.getRoots === 'function') getRoots = opts.getRoots
}

export function register(id, info) {
  meta.set(id, { ...info, exited: false })
  scrolls.set(id, '')
}
export function record(id, data) {
  if (!scrolls.has(id)) return
  const next = scrolls.get(id) + data
  scrolls.set(id, next.length > SCROLL_CAP ? next.slice(-SCROLL_CAP) : next)
}
export function markExited(id) {
  const m = meta.get(id)
  if (m) m.exited = true
}
export function forget(id) {
  meta.delete(id)
  scrolls.delete(id)
}
export function setPanes(list) {
  panes = Array.isArray(list) ? list : []
}
export function setAllowRun(v) {
  allowRun = !!v
}

const TOOLS = [
  {
    name: 'list_panes',
    description:
      'List every open pane in the workspace grid: id, tab title, and for terminal/agent panes the CLI kind, working directory, and whether the process is still alive.',
    input_schema: { type: 'object', properties: {} },
  },
  {
    name: 'read_terminal',
    description:
      'Read the recent output (scrollback tail) of a terminal or agent pane, ANSI-stripped. Use list_panes first to find the pane id.',
    input_schema: {
      type: 'object',
      properties: {
        pane_id: { type: 'string', description: 'pane id from list_panes' },
        lines: { type: 'number', description: 'how many trailing lines (default 60)' },
      },
      required: ['pane_id'],
    },
  },
  {
    name: 'type_in_terminal',
    description:
      'Type text into a terminal or agent pane. Set press_enter to also submit it — that only takes effect when the user has enabled "assistant may run commands"; otherwise the text is left in the prompt for the user to review and submit.',
    input_schema: {
      type: 'object',
      properties: {
        pane_id: { type: 'string' },
        text: { type: 'string' },
        press_enter: { type: 'boolean' },
      },
      required: ['pane_id', 'text'],
    },
  },
  {
    name: 'open_pane',
    // shared/pane-kinds.js owns the list — main's AGENTS and the renderer's
    // conductor:open switch read the same module
    description: 'Open a new pane in the grid. ' + OPENABLE_KINDS_DESCRIPTION,
    input_schema: {
      type: 'object',
      properties: { kind: { type: 'string' } },
      required: ['kind'],
    },
  },
  {
    name: 'open_file',
    description: 'Open a file from disk in an editor/viewer pane (absolute path).',
    input_schema: {
      type: 'object',
      properties: { path: { type: 'string' } },
      required: ['path'],
    },
  },
  {
    name: 'read_flow',
    description:
      'Read a saved flow. Without a name, lists the flows that exist in the workspace. With a name, returns the raw JSON of .tome/flows/<name>.flow.json.',
    input_schema: {
      type: 'object',
      properties: {
        name: { type: 'string', description: 'flow name without the .flow.json suffix' },
        root: { type: 'string', description: 'workspace folder (only needed when several are open)' },
      },
    },
  },
  {
    name: 'draft_flow',
    description:
      'Create or overwrite a flow at .tome/flows/<name>.flow.json; a flow pane opens and live-updates as you refine it. `flow` is the whole document: {version: 1, name, nodes: [], edges: []}. Node: {id, kind, name, instructions, expects, produces, inputs: [{name}], outputs: [{name}], x, y, model?} — kind is "terminal" or an agent CLI (' +
      AGENTS.join(', ') +
      '); give every node a unique short id like "n1"; omit x/y for auto-layout. Edge: {id, from, to, fromOutput, toInput} joining an output port name to an input port name. Structural errors are refused outright; contract warnings come back for you to raise with the user. Only call this after the user agrees to start (or change) a draft.',
    input_schema: {
      type: 'object',
      properties: {
        name: { type: 'string', description: 'flow name without the .flow.json suffix' },
        flow: { type: 'object', description: 'the entire flow document' },
        root: { type: 'string', description: 'workspace folder (only needed when several are open)' },
      },
      required: ['name', 'flow'],
    },
  },
]

export const SYSTEM =
  'You are the assistant pane inside Tome, a desktop coding harness whose grid holds ' +
  `terminal panes, agent CLI panes (${AGENTS.join(', ')}), editors, documents, and note vaults. ` +
  'You have tools to inspect and drive the workspace: list panes, read a terminal’s recent ' +
  'output, type into a terminal, open new panes or files. Use them whenever the user refers to ' +
  'other panes ("what is claude doing", "run the tests over there", "open a terminal"). ' +
  'type_in_terminal only submits when the user has enabled auto-run; otherwise the text is left ' +
  'for them to press Enter on — say so when it happens. ' +
  'Your replies may be read aloud, so keep them focused, brief, and speakable. ' +
  'Plain text only — no markdown tables. ' +
  'When the user wants to design a workflow, act as a flow architect. Flows are graphs of ' +
  'agent nodes saved as .tome/flows/<name>.flow.json; you shape them with read_flow and ' +
  'draft_flow. Restate the goal in one sentence, then ask one question at a time — never a ' +
  'questionnaire. Draft early and refine as you go: once the user agrees to start, call ' +
  'draft_flow as soon as a shape exists, then say what you added and what you assumed. ' +
  'Every node needs instructions, expects, and produces; a blank contract is a question to ' +
  'ask, not a field to invent, so challenge vagueness and voice every warning draft_flow ' +
  'returns. Never overwrite a flow you did not draft in this conversation without asking. ' +
  'You cannot run flows: when the user approves the final shape, say it is saved and that ' +
  'they press Run on the flow pane.'

// `chatId` rides along so the renderer can open what the assistant asks for
// as a tab in the requesting pane's own group instead of resplitting the grid.
function runTool(name, input, chatId) {
  switch (name) {
    case 'list_panes': {
      const rows = panes.map((p) => {
        const m = meta.get(p.id)
        return m
          ? { ...p, kind: m.kind, cwd: m.cwd, airgapped: m.airgap, alive: !m.exited && ptys.has(p.id) }
          : p
      })
      return JSON.stringify(rows)
    }
    case 'read_terminal': {
      const buf = scrolls.get(input.pane_id)
      if (buf == null) return 'No such terminal pane. Use list_panes.'
      const lines = stripAnsi(buf).split('\n')
      return lines.slice(-Math.min(Math.max(input.lines || 60, 1), 400)).join('\n') || '(no output yet)'
    }
    case 'type_in_terminal': {
      const p = ptys.get(input.pane_id)
      if (!p) return 'No such live terminal pane. Use list_panes.'
      const enter = !!input.press_enter && allowRun
      // With auto-run off the text must stay un-submitted, so strip the control
      // chars that would submit or signal on their own (CR/LF, Ctrl-C, Ctrl-D…).
      // Otherwise `text: "ls\r"` runs a command the user never approved.
      const text = allowRun ? String(input.text) : stripControlChars(input.text)
      p.write(text + (enter ? '\r' : ''))
      send('conductor:acted', { pane: input.pane_id, ran: enter })
      if (enter) return 'Typed and submitted.'
      return input.press_enter
        ? 'Typed, but NOT submitted: auto-run is disabled. The user can press Enter, or enable "assistant may run commands" in the ＋ menu.'
        : 'Typed (not submitted).'
    }
    case 'open_pane':
      send('conductor:open', { kind: String(input.kind || ''), source: chatId })
      return 'Requested.'
    case 'open_file': {
      const file = String(input.path || '')
      // The model must not make main open/parse arbitrary files on disk
      // (doc:read runs mammoth/SheetJS on whatever it points at) — only
      // paths inside the open workspace folders or a brain vault.
      if (!canOpenFile(file))
        return 'Refused: open_file is confined to the open workspace folders and brain vaults.'
      send('conductor:open', { file, source: chatId })
      return 'Requested.'
    }
    case 'read_flow':
      return readFlowTool(getRoots(), input)
    case 'draft_flow': {
      const { text, openPath } = draftFlowTool(getRoots(), input)
      // Open the pane only on create; overwrites reach the already-open pane
      // through the disk watcher, so re-opening would just churn the grid.
      if (openPath) send('conductor:open', { file: openPath, source: chatId })
      return text
    }
    default:
      return 'Unknown tool.'
  }
}

// Streamed chat with a bounded tool loop. Text deltas stream to the renderer
// as they arrive; tool calls surface as chat:tool events between segments.
//
// Budget: each tool turn re-sends the whole transcript, so 8 turns at
// max_tokens 64000 is a worst-case ~1M output tokens for one user message.
// TOKEN_BUDGET caps the cumulative usage across the loop; when exceeded we
// stop gracefully with a visible note instead of burning on silently.
//
// Abort: the renderer's stop button lands here via chat:abort; we abort the
// in-flight stream and end the loop after the current turn, flagging the
// done event so the renderer knows it was cancelled, not completed.
const TOKEN_BUDGET = 400_000
const inflight = new Map() // chatId -> AbortController

export function abortChat(id) {
  inflight.get(id)?.abort()
}

export async function runChat(anthropic, { id, model, system, messages, betas, fallbacks }) {
  const msgs = [...messages]
  const controller = new AbortController()
  inflight.set(id, controller)
  let aborted = false
  let totalTokens = 0
  try {
    for (let turn = 0; turn < 8; turn++) {
      if (controller.signal.aborted) break
      const args = {
        model,
        max_tokens: 64000,
        system: system || SYSTEM,
        messages: msgs,
        tools: TOOLS,
      }
      if (betas) args.betas = betas
      if (fallbacks) args.fallbacks = fallbacks
      const stream = anthropic.beta.messages.stream(args, { signal: controller.signal })
      stream.on('text', (text) => send('chat:delta', { id, text }))
      let final
      try {
        final = await stream.finalMessage()
      } catch (err) {
        if (controller.signal.aborted || err?.name === 'AbortError' || err?.name === 'APIUserAbortError') {
          aborted = true
          break
        }
        throw err
      }
      totalTokens += (final.usage?.input_tokens || 0) + (final.usage?.output_tokens || 0)
      if (final.stop_reason === 'refusal') {
        send('chat:done', { id, aborted: false, error: 'Request declined by safety classifiers.' })
        return
      }
      if (final.stop_reason !== 'tool_use') {
        send('chat:done', { id })
        return
      }
      msgs.push({ role: 'assistant', content: final.content })
      const results = []
      for (const block of final.content.filter((b) => b.type === 'tool_use')) {
        const hint = block.input?.pane_id || block.input?.kind || block.input?.path || block.input?.name || ''
        send('chat:tool', { id, tool: block.name, hint })
        let out
        let ok = true
        try {
          out = runTool(block.name, block.input || {}, id)
        } catch (err) {
          ok = false
          out = 'Tool error: ' + err.message
        }
        // Audit the ACTION only: tool name, chat, outcome, and the same
        // identifier hint the chat:tool event carries. Tool input/output
        // never goes in the log — typed text may contain secrets.
        logEvent('conductor:tool', { tool: block.name, chatId: id, ok, hint })
        results.push({ type: 'tool_result', tool_use_id: block.id, content: String(out) })
      }
      msgs.push({ role: 'user', content: results })
      if (totalTokens > TOKEN_BUDGET) {
        send('chat:done', {
          id,
          aborted: false,
          error: `Token budget reached (~${Math.round(totalTokens / 1000)}k tokens across tool turns) — stopped early. Ask again to continue.`,
        })
        return
      }
    }
    if (aborted || controller.signal.aborted) {
      send('chat:done', { id, aborted: true, error: 'Stopped.' })
      return
    }
    send('chat:done', { id, aborted: false, error: 'Tool loop limit reached — ask again to continue.' })
  } finally {
    inflight.delete(id)
  }
}
