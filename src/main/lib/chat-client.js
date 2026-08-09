// Normalized streaming chat client for the assistant pane — the wire
// boundary for WS-B's provider abstraction. The conductor keeps its history,
// tools, and system prompt in Anthropic shape (the canonical form); this
// module translates those shapes per provider and normalizes whatever comes
// back into
//   content: [{ type: 'text', text } | { type: 'tool_use', id, name, input }]
// with stopReason 'end' | 'tool_use' | 'refusal'.
// No new dependencies: the OpenAI-compatible wire (Kimi, GLM, custom
// endpoints) is plain fetch + hand-parsed SSE; the anthropic wire wraps the
// existing SDK stream.
import { CHAT_PROVIDERS, DEFAULT_CHAT_PROVIDER } from '../../shared/chat-providers.js'

// ---- provider resolution ----
// Order matters and preserves pre-existing behavior:
//   1. TOME_CHAT_BASE_URL / TOME_CHAT_MODEL env override — today's custom
//      endpoint path. anthropic wire when the URL lands on api.anthropic.com
//      or TOME_CHAT_WIRE=anthropic says so, else openai wire.
//   2. REQUESTY_API_KEY present → today's Requesty router verbatim (vertex
//      model id, beta:false — routers 400 on Anthropic-only beta args).
//   3. store key 'chat-provider' (validated against CHAT_PROVIDERS, default
//      kimi) + optional store key 'chat-model' override. The key comes from
//      the login-shell secrets map, falling back to the process env; a
//      missing key resolves to { keyMissing: entry } so the caller can name
//      the provider and the env var in the error.
// Requesty routes Claude via vertex/bedrock; bare anthropic/* ids 403 unless
// the key's Model Library approves them.
const REQUESTY_MODEL = 'vertex/claude-opus-4-8@eu'
const REQUESTY_BASE = 'https://router.requesty.ai'

export async function resolveChatProvider({ readStore, secrets = {} } = {}) {
  const read = typeof readStore === 'function' ? readStore : async () => undefined

  const envBase = process.env.TOME_CHAT_BASE_URL
  const envModel = process.env.TOME_CHAT_MODEL
  if (envBase || envModel) {
    const anthropicWire =
      process.env.TOME_CHAT_WIRE === 'anthropic' || (envBase || '').includes('api.anthropic.com')
    return {
      id: 'env',
      label: 'Custom endpoint (TOME_CHAT_BASE_URL/TOME_CHAT_MODEL)',
      wire: anthropicWire ? 'anthropic' : 'openai',
      opts: anthropicWire
        ? { baseURL: envBase || undefined }
        : {
            baseURL: envBase || 'https://api.anthropic.com',
            apiKey: secrets.ANTHROPIC_API_KEY || process.env.ANTHROPIC_API_KEY,
          },
      model: envModel || CHAT_PROVIDERS.claude.model,
      // Custom endpoints are not promised to accept Anthropic-only beta args.
      beta: false,
    }
  }

  const reqKey = process.env.REQUESTY_API_KEY || secrets.REQUESTY_API_KEY
  if (reqKey) {
    return {
      id: 'requesty',
      label: 'Requesty router',
      wire: 'anthropic',
      opts: { apiKey: reqKey, baseURL: REQUESTY_BASE },
      model: REQUESTY_MODEL,
      beta: false,
    }
  }

  const stored = await read('chat-provider')
  const id = CHAT_PROVIDERS[stored] ? stored : DEFAULT_CHAT_PROVIDER
  const entry = CHAT_PROVIDERS[id]
  const modelOverride = await read('chat-model')
  const apiKey = secrets[entry.keyEnv] || process.env[entry.keyEnv]
  if (!apiKey) return { keyMissing: entry, id }
  return {
    id,
    label: entry.label,
    wire: entry.wire,
    opts: entry.wire === 'openai' ? { baseURL: entry.baseURL, apiKey } : { apiKey },
    model:
      typeof modelOverride === 'string' && modelOverride.trim() ? modelOverride.trim() : entry.model,
    beta: entry.wire === 'anthropic',
  }
}

// ---- shape translation (pure; the conductor's shapes are canonical) ----

// Anthropic tool defs → OpenAI function defs. input_schema maps 1:1 onto
// parameters (both are plain JSON Schema).
export function toolsToOpenAI(anthropicTools) {
  return (anthropicTools || []).map((t) => ({
    type: 'function',
    function: { name: t.name, description: t.description, parameters: t.input_schema },
  }))
}

// Conductor history (Anthropic-shaped) → OpenAI messages:
//   assistant content blocks incl. tool_use → one assistant message with
//     tool_calls (arguments serialized to a JSON string)
//   user message whose content is tool_result blocks → one role:'tool'
//     message per result, keyed by tool_call_id
// Everything else (plain string content) passes through untouched.
export function openAIMessagesFrom(messages) {
  const out = []
  for (const m of messages || []) {
    if (m.role === 'assistant' && Array.isArray(m.content)) {
      const text = m.content
        .filter((b) => b.type === 'text')
        .map((b) => b.text)
        .join('')
      const calls = m.content
        .filter((b) => b.type === 'tool_use')
        .map((b) => ({
          id: b.id,
          type: 'function',
          function: { name: b.name, arguments: JSON.stringify(b.input ?? {}) },
        }))
      out.push({
        role: 'assistant',
        content: text || null,
        ...(calls.length ? { tool_calls: calls } : {}),
      })
    } else if (
      m.role === 'user' &&
      Array.isArray(m.content) &&
      m.content.some((b) => b && b.type === 'tool_result')
    ) {
      for (const b of m.content) {
        if (b.type !== 'tool_result') continue
        out.push({
          role: 'tool',
          tool_call_id: b.tool_use_id,
          content: typeof b.content === 'string' ? b.content : JSON.stringify(b.content),
        })
      }
    } else {
      out.push(m)
    }
  }
  return out
}

// ---- streaming ----

const FINISH_REASON = { stop: 'end', tool_calls: 'tool_use', content_filter: 'refusal' }

export async function streamChat({ provider, anthropic, system, messages, tools, signal, onText }) {
  if (provider.wire === 'anthropic') {
    return streamAnthropic({ provider, anthropic, system, messages, tools, signal, onText })
  }
  return streamOpenAI({ provider, system, messages, tools, signal, onText })
}

// Anthropic wire: the existing SDK path, verbatim — betas/fallbacks ride
// along on the provider so routers never see args they 400 on.
async function streamAnthropic({ provider, anthropic, system, messages, tools, signal, onText }) {
  const args = {
    model: provider.model,
    max_tokens: 64000,
    system,
    messages,
    tools,
  }
  if (provider.betas) args.betas = provider.betas
  if (provider.fallbacks) args.fallbacks = provider.fallbacks
  const stream = anthropic.beta.messages.stream(args, { signal })
  stream.on('text', onText)
  const final = await stream.finalMessage()
  return {
    stopReason:
      final.stop_reason === 'refusal'
        ? 'refusal'
        : final.stop_reason === 'tool_use'
          ? 'tool_use'
          : 'end',
    content: final.content,
    usage: {
      input: final.usage?.input_tokens || 0,
      output: final.usage?.output_tokens || 0,
    },
  }
}

// OpenAI-compatible wire: POST {baseURL}/chat/completions with stream:true,
// parse `data: {json}\n\n` lines until `data: [DONE]`. Tool calls stream as
// index-keyed fragments — arguments JSON arrives in pieces and must be
// accumulated per index before it can be parsed. The AbortSignal passes
// straight to fetch.
async function streamOpenAI({ provider, system, messages, tools, signal, onText }) {
  const res = await fetch(`${provider.opts.baseURL}/chat/completions`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      authorization: `Bearer ${provider.opts.apiKey || ''}`,
    },
    body: JSON.stringify({
      model: provider.model,
      stream: true,
      messages: system
        ? [{ role: 'system', content: system }, ...openAIMessagesFrom(messages)]
        : openAIMessagesFrom(messages),
      tools: toolsToOpenAI(tools),
    }),
    signal,
  })
  if (!res.ok) {
    const body = await res.text().catch(() => '')
    throw new Error(`chat: HTTP ${res.status} — ${body.slice(0, 200)}`)
  }

  const content = []
  const toolByIndex = new Map() // index -> { id, name, args } — args accumulates raw JSON text
  let usage = null
  let stopReason = 'end'
  let buf = ''

  const handleEvent = (data) => {
    if (data === '[DONE]') return
    let chunk
    try {
      chunk = JSON.parse(data)
    } catch {
      return // keepalive / comment line — the SSE spec allows both
    }
    if (chunk.usage) {
      usage = { input: chunk.usage.prompt_tokens || 0, output: chunk.usage.completion_tokens || 0 }
    }
    const choice = chunk.choices?.[0]
    if (!choice) return
    const delta = choice.delta || {}
    if (typeof delta.content === 'string' && delta.content) {
      onText(delta.content)
      const last = content[content.length - 1]
      if (last?.type === 'text') last.text += delta.content
      else content.push({ type: 'text', text: delta.content })
    }
    for (const call of delta.tool_calls || []) {
      const i = call.index ?? 0
      let acc = toolByIndex.get(i)
      if (!acc) toolByIndex.set(i, (acc = { id: '', name: '', args: '' }))
      if (call.id) acc.id += call.id
      if (call.function?.name) acc.name += call.function.name
      if (call.function?.arguments) acc.args += call.function.arguments
    }
    if (choice.finish_reason) stopReason = FINISH_REASON[choice.finish_reason] || 'end'
  }

  // SSE by hand: split on newlines, `data:` lines carry the payload; a read
  // chunk may split a line, so the tail rides the buffer to the next read.
  const reader = res.body.getReader()
  const decoder = new TextDecoder()
  for (;;) {
    const { done: eof, value } = await reader.read()
    if (eof) break
    buf += decoder.decode(value, { stream: true })
    const lines = buf.split('\n')
    buf = lines.pop()
    for (const line of lines) {
      const t = line.trim()
      if (t.startsWith('data:')) handleEvent(t.slice(5).trim())
    }
  }
  const tail = buf.trim()
  if (tail.startsWith('data:')) handleEvent(tail.slice(5).trim())

  for (const acc of toolByIndex.values()) {
    let input = {}
    try {
      input = acc.args ? JSON.parse(acc.args) : {}
    } catch {
      input = {} // unparseable arguments: surface an empty call, not a crash
    }
    content.push({ type: 'tool_use', id: acc.id, name: acc.name, input })
  }
  return { stopReason, content, usage: usage || { input: 0, output: 0 } }
}
