// Pure text sanitizers for pty scrollback and model-typed input.
// Extracted from conductor.js so the guards are testable without module state.

// CSI + OSC + stray escapes + control chars (keep \n and \t)
export const stripAnsi = (s) =>
  s
    .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, '')
    .replace(/\x1b\[[0-9;?]*[ -/]*[@-~]/g, '')
    .replace(/\x1b[@-_]/g, '')
    .replace(/[\x00-\x08\x0b-\x1f\x7f]/g, '')

// With auto-run off, model-typed text must stay un-submitted, so strip the
// control chars that would submit or signal on their own (CR/LF, Ctrl-C,
// Ctrl-D, ESC…). Tab survives — completion text is legitimate.
export const stripControlChars = (s) => String(s).replace(/[\x00-\x08\x0a-\x1f\x7f]/g, '')
