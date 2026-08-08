// Pins the conductor's text sanitizers — the auto-run control-char stripper
// is the guard that stops the chat model from submitting commands the user
// never approved (pi review §1). Verified correct today; pinned here.
import { describe, it, expect } from 'vitest'
import { stripAnsi, stripControlChars } from '../src/shared/terminal-text.js'

describe('stripAnsi', () => {
  it('removes CSI sequences', () => {
    expect(stripAnsi('\x1b[1;31mred\x1b[0m plain')).toBe('red plain')
    expect(stripAnsi('\x1b[2J\x1b[Hhome')).toBe('home')
    expect(stripAnsi('\x1b[?25lhidden-cursor')).toBe('hidden-cursor')
  })

  it('removes OSC sequences (BEL and ST terminated)', () => {
    expect(stripAnsi('\x1b]0;window title\x07after')).toBe('after')
    expect(stripAnsi('\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\')).toBe('link')
  })

  it('removes stray escapes and control chars', () => {
    expect(stripAnsi('a\x1bM b\x07c\x00d\x7fe')).toBe('a bcde')
  })

  it('preserves newline and tab', () => {
    expect(stripAnsi('line1\nline2\tcol')).toBe('line1\nline2\tcol')
  })
})

describe('stripControlChars (auto-run guard)', () => {
  it('removes every submission/signal character', () => {
    expect(stripControlChars('ls\r')).toBe('ls') // CR — submit
    expect(stripControlChars('ls\n')).toBe('ls') // LF — submit
    expect(stripControlChars('a\x03b')).toBe('ab') // Ctrl-C — SIGINT
    expect(stripControlChars('a\x04b')).toBe('ab') // Ctrl-D — EOF
    expect(stripControlChars('a\x1bb')).toBe('ab') // ESC — readline escape
    expect(stripControlChars('a\x7fb')).toBe('ab') // DEL
    expect(stripControlChars('a\x00b')).toBe('ab') // NUL
  })

  it('preserves tab (completion text is legitimate)', () => {
    expect(stripControlChars('git che\t')).toBe('git che\t')
  })

  it('strips CR/LF out of multi-line smuggle attempts', () => {
    expect(stripControlChars('echo hi\r\nrm -rf ~\r')).toBe('echo hirm -rf ~')
  })

  it('coerces non-strings and leaves plain text untouched', () => {
    expect(stripControlChars(42)).toBe('42')
    expect(stripControlChars('plain text 123 !@#')).toBe('plain text 123 !@#')
  })
})
