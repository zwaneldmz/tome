// Pins the assistant provider abstraction (WS-B): the Anthropic↔OpenAI
// shape translators against the conductor's real TOOLS and a recorded
// tool-loop transcript, hand-parsed SSE streaming (fragmented tool_calls,
// finish_reason mapping, HTTP errors), and resolveChatProvider's ordering
// (env override > Requesty > store > default, keyMissing path).
// fetch is mocked throughout — these tests never touch the network.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { CHAT_PROVIDERS, DEFAULT_CHAT_PROVIDER } from '../src/shared/chat-providers.js'
import {
  resolveChatProvider,
  toolsToOpenAI,
  openAIMessagesFrom,
  streamChat,
} from '../src/main/lib/chat-client.js'
import { TOOLS } from '../src/main/conductor.js'

// The env vars resolveChatProvider reads, scrubbed around every test so one
// case's override can't leak into the next.
const ENV_KEYS = [
  'TOME_CHAT_BASE_URL',
  'TOME_CHAT_MODEL',
  'TOME_CHAT_WIRE',
  'REQUESTY_API_KEY',
  'MOONSHOT_API_KEY',
  'ZHIPU_API_KEY',
  'ANTHROPIC_API_KEY',
]
const savedEnv = {}
for (const k of ENV_KEYS) savedEnv[k] = process.env[k]
function scrubEnv() {
  for (const k of ENV_KEYS) delete process.env[k]
}
// Scrub BEFORE each test too — the developer's own shell may export these
// (REQUESTY_API_KEY etc.), and resolution order tests must start from zero.
beforeEach(scrubEnv)
afterEach(() => {
  scrubEnv()
  for (const k of ENV_KEYS) if (savedEnv[k] !== undefined) process.env[k] = savedEnv[k]
  vi.unstubAllGlobals()
})

// A canned SSE response: chunks joined into one ReadableStream body, exactly
// what fetch would hand back from a streaming endpoint.
function sseResponse(chunks, { status = 200, tail = 'data: [DONE]\n\n' } = {}) {
  const body = chunks.map((c) => `data: ${JSON.stringify(c)}\n\n`).join('') + tail
  return {
    ok: status >= 200 && status < 300,
    status,
    text: async () => body,
    body: new ReadableStream({
      start(c) {
        c.enqueue(new TextEncoder().encode(body))
        c.close()
      },
    }),
  }
}

describe('toolsToOpenAI', () => {
  it('round-trips the conductor’s real TOOLS array', () => {
    const out = toolsToOpenAI(TOOLS)
    expect(out).toHaveLength(TOOLS.length)
    for (let i = 0; i < TOOLS.length; i++) {
      expect(out[i].type).toBe('function')
      expect(out[i].function.name).toBe(TOOLS[i].name)
      expect(out[i].function.description).toBe(TOOLS[i].description)
      // input_schema maps 1:1 onto parameters — both plain JSON Schema
      expect(out[i].function.parameters).toEqual(TOOLS[i].input_schema)
    }
    // spot-check a required-args tool survives intact
    const read = out.find((t) => t.function.name === 'read_terminal')
    expect(read.function.parameters.required).toEqual(['pane_id'])
  })

  it('tolerates empty/undefined tool lists', () => {
    expect(toolsToOpenAI(undefined)).toEqual([])
    expect(toolsToOpenAI([])).toEqual([])
  })
})

describe('openAIMessagesFrom', () => {
  // The exact shape conductor.runChat produces across one tool loop:
  // user text → assistant text+tool_use → user tool_result → assistant text.
  const transcript = [
    { role: 'user', content: 'what is claude doing?' },
    {
      role: 'assistant',
      content: [
        { type: 'text', text: 'Let me look.' },
        { type: 'tool_use', id: 'toolu_1', name: 'list_panes', input: {} },
      ],
    },
    {
      role: 'user',
      content: [{ type: 'tool_result', tool_use_id: 'toolu_1', content: '[{"id":"p1"}]' }],
    },
    { role: 'assistant', content: [{ type: 'text', text: 'Claude is editing a file.' }] },
  ]

  it('converts a recorded tool-loop transcript', () => {
    const out = openAIMessagesFrom(transcript)
    expect(out).toEqual([
      { role: 'user', content: 'what is claude doing?' },
      {
        role: 'assistant',
        content: 'Let me look.',
        tool_calls: [
          {
            id: 'toolu_1',
            type: 'function',
            function: { name: 'list_panes', arguments: '{}' },
          },
        ],
      },
      { role: 'tool', tool_call_id: 'toolu_1', content: '[{"id":"p1"}]' },
      { role: 'assistant', content: 'Claude is editing a file.' },
    ])
  })

  it('emits one role:tool message per tool_result block', () => {
    const out = openAIMessagesFrom([
      {
        role: 'user',
        content: [
          { type: 'tool_result', tool_use_id: 'a', content: 'one' },
          { type: 'tool_result', tool_use_id: 'b', content: 'two' },
        ],
      },
    ])
    expect(out).toEqual([
      { role: 'tool', tool_call_id: 'a', content: 'one' },
      { role: 'tool', tool_call_id: 'b', content: 'two' },
    ])
  })

  it('serializes tool_use input and nulls textless assistant content', () => {
    const out = openAIMessagesFrom([
      {
        role: 'assistant',
        content: [{ type: 'tool_use', id: 'x', name: 'read_terminal', input: { pane_id: 'p1' } }],
      },
    ])
    expect(out[0].content).toBeNull()
    expect(out[0].tool_calls[0].function.arguments).toBe('{"pane_id":"p1"}')
  })

  it('passes plain string messages through untouched', () => {
    const msgs = [{ role: 'user', content: 'hi' }]
    expect(openAIMessagesFrom(msgs)).toEqual(msgs)
  })
})

describe('streamChat (openai wire)', () => {
  const provider = {
    wire: 'openai',
    opts: { baseURL: 'https://api.moonshot.ai/v1', apiKey: 'sk-test' },
    model: 'kimi-k3',
  }

  it('streams text deltas and maps finish_reason stop → end', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        sseResponse([
          { choices: [{ delta: { role: 'assistant', content: '' } }] },
          { choices: [{ delta: { content: 'Hel' } }] },
          { choices: [{ delta: { content: 'lo' } }] },
          { choices: [{ delta: {}, finish_reason: 'stop' }] },
        ])
      )
    )
    const texts = []
    const res = await streamChat({
      provider,
      system: 'sys',
      messages: [{ role: 'user', content: 'hi' }],
      tools: TOOLS,
      signal: new AbortController().signal,
      onText: (t) => texts.push(t),
    })
    expect(texts).toEqual(['Hel', 'lo'])
    expect(res.stopReason).toBe('end')
    expect(res.content).toEqual([{ type: 'text', text: 'Hello' }])
    // request shape: system first, OpenAI function tools, bearer auth
    const [url, init] = fetch.mock.calls[0]
    expect(url).toBe('https://api.moonshot.ai/v1/chat/completions')
    expect(init.headers.authorization).toBe('Bearer sk-test')
    const sent = JSON.parse(init.body)
    expect(sent.stream).toBe(true)
    expect(sent.messages[0]).toEqual({ role: 'system', content: 'sys' })
    expect(sent.tools[0].type).toBe('function')
  })

  it('accumulates fragmented tool_calls per index and parses arguments', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        sseResponse([
          { choices: [{ delta: { content: 'Checking.' } }] },
          {
            choices: [
              {
                delta: {
                  tool_calls: [
                    { index: 0, id: 'call_1', function: { name: 'read_terminal', arguments: '' } },
                  ],
                },
              },
            ],
          },
          {
            choices: [
              { delta: { tool_calls: [{ index: 0, function: { arguments: '{"pane_' } }] } },
            ],
          },
          {
            choices: [
              { delta: { tool_calls: [{ index: 0, function: { arguments: 'id":"p1"}' } }] } },
            ],
          },
          {
            choices: [
              {
                delta: {
                  tool_calls: [
                    { index: 1, id: 'call_2', function: { name: 'list_panes', arguments: '{}' } },
                  ],
                },
              },
            ],
          },
          { choices: [{ delta: {}, finish_reason: 'tool_calls' }] },
          { usage: { prompt_tokens: 11, completion_tokens: 7 } },
        ])
      )
    )
    const res = await streamChat({
      provider,
      system: null,
      messages: [{ role: 'user', content: 'hi' }],
      tools: TOOLS,
      signal: new AbortController().signal,
      onText: () => {},
    })
    expect(res.stopReason).toBe('tool_use')
    expect(res.content).toEqual([
      { type: 'text', text: 'Checking.' },
      { type: 'tool_use', id: 'call_1', name: 'read_terminal', input: { pane_id: 'p1' } },
      { type: 'tool_use', id: 'call_2', name: 'list_panes', input: {} },
    ])
    expect(res.usage).toEqual({ input: 11, output: 7 })
  })

  it('maps content_filter → refusal', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        sseResponse([{ choices: [{ delta: { content: 'I can' }, finish_reason: 'content_filter' }] }])
      )
    )
    const res = await streamChat({
      provider,
      system: null,
      messages: [],
      tools: [],
      signal: new AbortController().signal,
      onText: () => {},
    })
    expect(res.stopReason).toBe('refusal')
  })

  it('throws with status + body snippet on non-2xx', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({
        ok: false,
        status: 401,
        text: async () => '{"error":"bad key"}',
        body: null,
      }))
    )
    await expect(
      streamChat({
        provider,
        system: null,
        messages: [],
        tools: [],
        signal: new AbortController().signal,
        onText: () => {},
      })
    ).rejects.toThrow('chat: HTTP 401 — {"error":"bad key"}')
  })

  it('survives a chunk boundary splitting an SSE line', async () => {
    const raw =
      'data: {"choices":[{"delta":{"content":"sp'
    const raw2 = 'lit"}}]}\n\ndata: [DONE]\n\n'
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({
        ok: true,
        status: 200,
        body: new ReadableStream({
          start(c) {
            const enc = new TextEncoder()
            c.enqueue(enc.encode(raw))
            c.enqueue(enc.encode(raw2))
            c.close()
          },
        }),
      }))
    )
    const texts = []
    const res = await streamChat({
      provider,
      system: null,
      messages: [],
      tools: [],
      signal: new AbortController().signal,
      onText: (t) => texts.push(t),
    })
    expect(texts).toEqual(['split'])
    expect(res.content).toEqual([{ type: 'text', text: 'split' }])
  })
})

describe('streamChat (anthropic wire)', () => {
  it('wraps the SDK stream and normalizes stop reasons', async () => {
    const listeners = {}
    const anthropic = {
      beta: {
        messages: {
          stream: (args) => {
            expect(args.model).toBe('claude-opus-5')
            expect(args.betas).toEqual(['server-side-fallback-2026-07-01'])
            return {
              on: (ev, cb) => {
                listeners[ev] = cb
              },
              finalMessage: async () => {
                listeners.text('hi ')
                listeners.text('there')
                return {
                  stop_reason: 'end_turn',
                  content: [{ type: 'text', text: 'hi there' }],
                  usage: { input_tokens: 3, output_tokens: 2 },
                }
              },
            }
          },
        },
      },
    }
    const texts = []
    const res = await streamChat({
      provider: {
        wire: 'anthropic',
        model: 'claude-opus-5',
        betas: ['server-side-fallback-2026-07-01'],
      },
      anthropic,
      system: 'sys',
      messages: [],
      tools: TOOLS,
      signal: new AbortController().signal,
      onText: (t) => texts.push(t),
    })
    expect(texts).toEqual(['hi ', 'there'])
    expect(res.stopReason).toBe('end')
    expect(res.usage).toEqual({ input: 3, output: 2 })
  })
})

describe('resolveChatProvider', () => {
  const storeReader = (map) => async (key) => map[key] ?? null

  it('env override wins over everything, openai wire by default', async () => {
    process.env.TOME_CHAT_BASE_URL = 'http://localhost:1234/v1'
    process.env.TOME_CHAT_MODEL = 'local-model'
    process.env.REQUESTY_API_KEY = 'rq'
    const p = await resolveChatProvider({
      readStore: storeReader({ 'chat-provider': 'claude' }),
      secrets: { ANTHROPIC_API_KEY: 'sk' },
    })
    expect(p.wire).toBe('openai')
    expect(p.opts.baseURL).toBe('http://localhost:1234/v1')
    expect(p.model).toBe('local-model')
    expect(p.beta).toBe(false)
  })

  it('env override on api.anthropic.com (or TOME_CHAT_WIRE) is anthropic wire', async () => {
    process.env.TOME_CHAT_BASE_URL = 'https://api.anthropic.com'
    const p = await resolveChatProvider({ readStore: storeReader({}), secrets: {} })
    expect(p.wire).toBe('anthropic')
    process.env.TOME_CHAT_WIRE = 'anthropic'
    const p2 = await resolveChatProvider({ readStore: storeReader({}), secrets: {} })
    expect(p2.wire).toBe('anthropic')
  })

  it('Requesty key beats the store and keeps the vertex model verbatim', async () => {
    process.env.REQUESTY_API_KEY = 'rq-key'
    const p = await resolveChatProvider({
      readStore: storeReader({ 'chat-provider': 'glm' }),
      secrets: {},
    })
    expect(p.wire).toBe('anthropic')
    expect(p.opts).toEqual({ apiKey: 'rq-key', baseURL: 'https://router.requesty.ai' })
    expect(p.model).toBe('vertex/claude-opus-4-8@eu')
    expect(p.beta).toBe(false)
  })

  it('store provider + login-shell key resolves that provider', async () => {
    const p = await resolveChatProvider({
      readStore: storeReader({ 'chat-provider': 'glm', 'chat-model': 'glm-custom' }),
      secrets: { ZHIPU_API_KEY: 'z-key' },
    })
    expect(p.id).toBe('glm')
    expect(p.wire).toBe('openai')
    expect(p.opts).toEqual({ baseURL: CHAT_PROVIDERS.glm.baseURL, apiKey: 'z-key' })
    expect(p.model).toBe('glm-custom')
  })

  it('defaults to kimi with its default model when the store is empty', async () => {
    const p = await resolveChatProvider({
      readStore: storeReader({}),
      secrets: { MOONSHOT_API_KEY: 'm-key' },
    })
    expect(p.id).toBe(DEFAULT_CHAT_PROVIDER)
    expect(p.model).toBe(CHAT_PROVIDERS.kimi.model)
    expect(p.opts.baseURL).toBe(CHAT_PROVIDERS.kimi.baseURL)
  })

  it('an invalid stored provider falls back to the default', async () => {
    const p = await resolveChatProvider({
      readStore: storeReader({ 'chat-provider': 'bogus' }),
      secrets: { MOONSHOT_API_KEY: 'm-key' },
    })
    expect(p.id).toBe(DEFAULT_CHAT_PROVIDER)
  })

  it('missing key resolves to keyMissing naming the provider entry', async () => {
    const p = await resolveChatProvider({ readStore: storeReader({}), secrets: {} })
    expect(p.keyMissing).toBe(CHAT_PROVIDERS.kimi)
    expect(p.id).toBe('kimi')
  })

  it('claude resolves anthropic wire with beta on', async () => {
    const p = await resolveChatProvider({
      readStore: storeReader({ 'chat-provider': 'claude' }),
      secrets: { ANTHROPIC_API_KEY: 'a-key' },
    })
    expect(p.wire).toBe('anthropic')
    expect(p.beta).toBe(true)
    expect(p.model).toBe(CHAT_PROVIDERS.claude.model)
  })
})
