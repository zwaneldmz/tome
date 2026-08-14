// Renders docx/xlsx bytes to sandboxed-iframe-ready HTML entirely in the
// renderer. Ports src/main/index.js's `docCss()` helper and the mammoth/
// SheetJS half of its `doc:read` handler — moved out of the privileged
// process per the plan (mammoth/SheetJS both have CVE histories; running
// them where a prompt-injected file can reach nothing but this renderer's
// own sandboxed iframe is strictly safer than running them in Electron main
// or a Tauri command). The confinement half of that handler stays in Rust
// (`src-tauri/src/ipc/doc.rs`'s `doc_read_bytes`) — this module only ever
// sees bytes it's already been handed.
//
// Both libraries are dynamic-imported, lazily and promise-cached exactly
// like the original's loadMammoth()/loadXlsx(): the first docx/xlsx open of
// a session pays the module load, every later one reuses it, and a session
// that never opens either format never pays it at all.

let mammothPromise = null
const loadMammoth = () => (mammothPromise ??= import('mammoth'))
let xlsxPromise = null
const loadXlsx = () => (xlsxPromise ??= import('xlsx'))

// Styles injected into sandboxed doc-viewer iframes (docx/xlsx conversions)
// — ports index.js's docCss(). `dark` replaces that function's module-level
// `uiTheme` read: this runs in the renderer now, which already tracks the
// resolved theme itself (see theme.js's `isDark()`), so the caller passes
// it straight through instead of this module keeping its own copy.
export function docCss(dark) {
  const bg = dark ? '#0b0b11' : '#ffffff'
  const fg = dark ? '#c9d4e3' : '#35353d'
  const head = dark ? '#eef4fb' : '#101014'
  const link = dark ? '#00e5ff' : '#0071e3'
  const line = dark ? 'rgba(255,255,255,0.12)' : 'rgba(0,0,0,0.12)'
  const zebra = dark ? '#151723' : '#f1f1f5'
  return (
    `<style>body{font:14px/1.65 -apple-system,BlinkMacSystemFont,system-ui,sans-serif;` +
    `background:${bg};color:${fg};padding:30px;max-width:840px;margin:0 auto}` +
    `h1,h2,h3{color:${head}}a{color:${link}}` +
    'table{border-collapse:collapse;font-size:12.5px;font-family:ui-monospace,Menlo,monospace}' +
    `td,th{border:1px solid ${line};padding:4px 10px;white-space:nowrap}th{background:${zebra}}` +
    'img{max-width:100%}</style>'
  )
}

export const escapeHtml = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;')

// Standard-alphabet base64 -> Uint8Array. atob() is available in every
// renderer context Tome ships to (Chromium via Electron, WebKitGTK/WKWebView
// via Tauri) — no extra dependency for the decode half of the base64 encode
// Rust's `doc_read_bytes` does on the way out (see that command's own doc
// comment on why it hand-rolls the encoder rather than adding a crate).
export function base64ToBytes(b64) {
  const bin = atob(b64)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
  return bytes
}

// ext is WITHOUT the leading dot, lowercase (docx | xlsx | xls — the same
// set panes.js's CONV_EXT already restricts `mode: 'doc'` panes to, and
// doc_read_bytes's own DOC_EXTENSIONS restricts on the Rust side). Throws
// the same 'No viewer for .<ext>' shape index.js's doc:read handler used
// to, for DocPanel's existing catch-and-fallback to reuse verbatim. The
// extension check runs before either dynamic import, so an unsupported ext
// never pays for (or even touches) mammoth/xlsx's module load.
export async function convertToHtml(ext, bytes, dark) {
  if (ext !== 'docx' && ext !== 'xlsx' && ext !== 'xls') {
    throw new Error('No viewer for .' + ext)
  }
  if (ext === 'docx') {
    const mammoth = await loadMammoth()
    // mammoth's own `openZip` (lib/unzip.js) only recognizes `path`/
    // `buffer`/`file` options — NOT `arrayBuffer`; passing `{ arrayBuffer }`
    // silently falls through to its `else` branch and rejects every call
    // with "Could not find file in options", regardless of how well-formed
    // `bytes` is. `buffer` is handed straight to JSZip.loadAsync, which
    // accepts a Uint8Array directly (no `.buffer` unwrap needed).
    const { value } = await mammoth.convertToHtml({ buffer: bytes })
    return docCss(dark) + value
  }
  const XLSX = await loadXlsx()
  const wb = XLSX.read(bytes, { type: 'array' })
  const parts = wb.SheetNames.map(
    (n) => `<h3>${escapeHtml(n)}</h3>` + XLSX.utils.sheet_to_html(wb.Sheets[n], { header: '', footer: '' })
  )
  return docCss(dark) + parts.join('')
}
