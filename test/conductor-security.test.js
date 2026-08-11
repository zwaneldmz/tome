// TOME-009 (pane-scoped scrollback consent) + TOME-015 (abort the provider
// and tool loop on pane disposal), both in src/main/conductor.js. fetch is
// mocked for the abort test — never touches the network — same pattern as
// test/chat-providers.test.js.
import { describe, it, expect, vi, afterEach } from 'vitest'
import * as conductor from '../src/main/conductor.js'

afterEach(() => {
  vi.unstubAllGlobals()
})

// A canned SSE response: chunks joined into one ReadableStream body, exactly
// what fetch would hand back from a streaming endpoint (mirrors the helper
// in test/chat-providers.test.js).
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

describe('conductor read_terminal (TOME-009 pane-scoped consent)', () => {
  it('refuses a registered pane with no consent granted', () => {
    conductor.init({ send: vi.fn(), logEvent: vi.fn(), ptys: new Map() })
    conductor.register('p-noconsent', { kind: 'terminal', cwd: '/tmp', airgap: false })
    conductor.record('p-noconsent', 'hello world')
    const out = conductor.runTool('read_terminal', { pane_id: 'p-noconsent' }, 'chat-1')
    expect(out).toBe('Refused: user has not authorized reading this terminal.')
  })

  it('surfaces a one-time consent prompt for an unconsented pane, not one per call', () => {
    const send = vi.fn()
    conductor.init({ send, logEvent: vi.fn(), ptys: new Map() })
    conductor.register('p-ask', { kind: 'terminal', cwd: '/tmp', airgap: false })
    conductor.record('p-ask', 'hello world')
    conductor.runTool('read_terminal', { pane_id: 'p-ask' }, 'chat-1')
    conductor.runTool('read_terminal', { pane_id: 'p-ask' }, 'chat-1')
    const asks = send.mock.calls.filter(([ch]) => ch === 'conductor:readRequest')
    expect(asks).toEqual([['conductor:readRequest', { paneId: 'p-ask' }]])
  })

  it('never prompts for an air-gapped pane — it is refused outright', () => {
    const send = vi.fn()
    conductor.init({ send, logEvent: vi.fn(), ptys: new Map() })
    conductor.register('p-air', { kind: 'terminal', cwd: '/tmp', airgap: true })
    conductor.record('p-air', 'secret')
    conductor.runTool('read_terminal', { pane_id: 'p-air' }, 'chat-1')
    expect(send.mock.calls.some(([ch]) => ch === 'conductor:readRequest')).toBe(false)
  })

  it('refuses an air-gapped pane even after consent is granted', () => {
    conductor.init({ send: vi.fn(), logEvent: vi.fn(), ptys: new Map() })
    conductor.register('p-airgap', { kind: 'terminal', cwd: '/tmp', airgap: true })
    conductor.record('p-airgap', 'secret output')
    conductor.setReadConsent('p-airgap', true)
    const out = conductor.runTool('read_terminal', { pane_id: 'p-airgap' }, 'chat-1')
    expect(out).toBe('Refused: air-gapped pane output cannot be disclosed.')
  })

  it('returns scrollback once consented on a non-airgapped pane, and audits pane+count only', () => {
    const logEvent = vi.fn()
    conductor.init({ send: vi.fn(), logEvent, ptys: new Map() })
    conductor.register('p-ok', { kind: 'terminal', cwd: '/tmp', airgap: false })
    conductor.record('p-ok', 'line1\nline2\nline3')
    conductor.setReadConsent('p-ok', true)
    const out = conductor.runTool('read_terminal', { pane_id: 'p-ok', lines: 2 }, 'chat-1')
    expect(out).toBe('line2\nline3')
    // Audit carries the pane id and the line count only — never the content.
    expect(logEvent).toHaveBeenCalledTimes(1)
    expect(logEvent).toHaveBeenCalledWith('conductor:read', { paneId: 'p-ok', lines: 2 })
  })

  it('revoking consent refuses again', () => {
    conductor.init({ send: vi.fn(), logEvent: vi.fn(), ptys: new Map() })
    conductor.register('p-revoke', { kind: 'terminal', cwd: '/tmp', airgap: false })
    conductor.record('p-revoke', 'x')
    conductor.setReadConsent('p-revoke', true)
    conductor.setReadConsent('p-revoke', false)
    const out = conductor.runTool('read_terminal', { pane_id: 'p-revoke' }, 'chat-1')
    expect(out).toBe('Refused: user has not authorized reading this terminal.')
  })

  it('an unknown pane is still refused for missing-pane reasons, not consent', () => {
    conductor.init({ send: vi.fn(), logEvent: vi.fn(), ptys: new Map() })
    const out = conductor.runTool('read_terminal', { pane_id: 'never-registered' }, 'chat-1')
    expect(out).toBe('No such terminal pane. Use list_panes.')
  })

  it('forgetting a pane clears its read consent too', () => {
    conductor.init({ send: vi.fn(), logEvent: vi.fn(), ptys: new Map() })
    conductor.register('p-forget', { kind: 'terminal', cwd: '/tmp', airgap: false })
    conductor.record('p-forget', 'data')
    conductor.setReadConsent('p-forget', true)
    conductor.forget('p-forget')
    conductor.register('p-forget', { kind: 'terminal', cwd: '/tmp', airgap: false })
    conductor.record('p-forget', 'data-again')
    const out = conductor.runTool('read_terminal', { pane_id: 'p-forget' }, 'chat-1')
    expect(out).toBe('Refused: user has not authorized reading this terminal.')
  })
})

describe('conductor runChat abort (TOME-015)', () => {
  it('stops the tool loop mid-batch and emits one terminal chat:done with aborted:true', async () => {
    // Two tool_use blocks in a single assistant turn. The mock `send` aborts
    // as soon as the FIRST chat:tool event fires — synchronously, so the
    // abort lands before the loop reaches the second block.
    vi.stubGlobal(
      'fetch',
      vi.fn(async () =>
        sseResponse([
          { choices: [{ delta: { role: 'assistant', content: '' } }] },
          {
            choices: [
              {
                delta: {
                  tool_calls: [
                    { index: 0, id: 'call_1', function: { name: 'list_panes', arguments: '{}' } },
                  ],
                },
              },
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
        ])
      )
    )
    const chatId = 'abort-test-1'
    const sent = []
    let chatToolCount = 0
    const send = vi.fn((channel, payload) => {
      sent.push([channel, payload])
      if (channel === 'chat:tool') {
        chatToolCount++
        if (chatToolCount === 1) conductor.abortChat(chatId)
      }
    })
    const logEvent = vi.fn()
    conductor.init({ send, logEvent, ptys: new Map() })

    const client = {
      wire: 'openai',
      opts: { baseURL: 'https://api.example.test/v1', apiKey: 'sk-test' },
      model: 'test-model',
    }
    await conductor.runChat({
      id: chatId,
      system: 'sys',
      messages: [{ role: 'user', content: 'do stuff' }],
      client,
    })

    // Only the first tool ran — the second never got a chat:tool event, a
    // runTool call, or an audit entry.
    expect(chatToolCount).toBe(1)
    expect(logEvent).toHaveBeenCalledTimes(1)
    expect(logEvent).toHaveBeenCalledWith(
      'conductor:tool',
      expect.objectContaining({ tool: 'list_panes', chatId, ok: true })
    )
    // The loop stopped instead of re-sending the transcript for another turn.
    expect(fetch).toHaveBeenCalledTimes(1)
    // Exactly one terminal outcome, flagged aborted.
    const doneEvents = sent.filter(([channel]) => channel === 'chat:done')
    expect(doneEvents).toHaveLength(1)
    expect(doneEvents[0][1]).toEqual({ id: chatId, aborted: true, error: 'Stopped.' })
  })
})
