// Pure text sanitizers for pty scrollback and typed input. Shared — not
// main-only — because both main (conductor's type_in_terminal/read_terminal)
// and the renderer (flow.js's Run, via typeIntoPanel in panes.js) need the
// exact same no-auto-submit guard; a second, drifted copy of the
// control-char regex would be the kind of thing that quietly stops matching
// a new bypass in only one of the two places.

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
