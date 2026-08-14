// Pins the renderer-side doc conversion helpers (src/renderer/doc-convert.js)
// that moved out of Electron main per the phase 5a-docs task: docCss's exact
// output (the sandboxed iframe's only styling — a drift here is a visible
// regression), base64ToBytes (the decode half of Rust's doc_read_bytes
// base64 encode — see src-tauri/src/ipc/doc.rs's own RFC 4648 vector tests
// for the encode half), convertToHtml's extension guard (reachable without
// either library ever loading), and — unlike an earlier version of this
// comment claimed — convertToHtml's real docx/xlsx conversion branches
// themselves: both mammoth and xlsx import and run correctly under vitest's
// plain-Node module resolution (verified directly: a probe importing each
// and driving convertToHtml with real bytes worked end to end), so there was
// never a resolution barrier here, only a missing fixture. xlsx is
// fixture-built in-memory below (XLSX.utils.book_new/aoa_to_sheet); docx
// reuses mammoth's own tiny "single-paragraph.docx" test fixture (BSD-2-
// Clause, copied from node_modules/mammoth/test/test-data into
// test/fixtures so this suite doesn't depend on node_modules' internal
// layout at run time) rather than hand-rolling a ZIP writer just for this.
import { describe, it, expect } from 'vitest'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import * as XLSX from 'xlsx'
import { docCss, escapeHtml, base64ToBytes, convertToHtml } from '../src/renderer/doc-convert.js'

const fixture = (name) => readFileSync(fileURLToPath(new URL(`fixtures/${name}`, import.meta.url)))

describe('docCss', () => {
  it('dark: exact style block, port of index.js docCss() with uiTheme === "dark"', () => {
    expect(docCss(true)).toBe(
      '<style>body{font:14px/1.65 -apple-system,BlinkMacSystemFont,system-ui,sans-serif;' +
        'background:#0b0b11;color:#c9d4e3;padding:30px;max-width:840px;margin:0 auto}' +
        'h1,h2,h3{color:#eef4fb}a{color:#00e5ff}' +
        'table{border-collapse:collapse;font-size:12.5px;font-family:ui-monospace,Menlo,monospace}' +
        'td,th{border:1px solid rgba(255,255,255,0.12);padding:4px 10px;white-space:nowrap}th{background:#151723}' +
        'img{max-width:100%}</style>'
    )
  })

  it('light: exact style block, port of index.js docCss() with uiTheme !== "dark"', () => {
    expect(docCss(false)).toBe(
      '<style>body{font:14px/1.65 -apple-system,BlinkMacSystemFont,system-ui,sans-serif;' +
        'background:#ffffff;color:#35353d;padding:30px;max-width:840px;margin:0 auto}' +
        'h1,h2,h3{color:#101014}a{color:#0071e3}' +
        'table{border-collapse:collapse;font-size:12.5px;font-family:ui-monospace,Menlo,monospace}' +
        'td,th{border:1px solid rgba(0,0,0,0.12);padding:4px 10px;white-space:nowrap}th{background:#f1f1f5}' +
        'img{max-width:100%}</style>'
    )
  })
})

describe('escapeHtml', () => {
  it('escapes & and < only, matching the sheet-name escaper from index.js', () => {
    expect(escapeHtml('Q1 & Q2 <2024>')).toBe('Q1 &amp; Q2 &lt;2024>')
  })
})

describe('base64ToBytes', () => {
  // Same RFC 4648 §10 vectors src-tauri/src/ipc/doc.rs's base64_encode
  // tests pin on the encode side — decoding them here checks the two ends
  // of the wire agree on the same alphabet without needing a live backend.
  it.each([
    ['', ''],
    ['Zg==', 'f'],
    ['Zm8=', 'fo'],
    ['Zm9v', 'foo'],
    ['Zm9vYg==', 'foob'],
    ['Zm9vYmE=', 'fooba'],
    ['Zm9vYmFy', 'foobar'],
  ])('decodes %s to the bytes of %j', (b64, text) => {
    const bytes = base64ToBytes(b64)
    expect(Array.from(bytes)).toEqual(Array.from(text).map((c) => c.charCodeAt(0)))
  })

  it('round-trips arbitrary bytes through btoa/atob', () => {
    const original = new Uint8Array([0, 1, 2, 253, 254, 255, 16, 32, 127, 128])
    const b64 = btoa(String.fromCharCode(...original))
    expect(Array.from(base64ToBytes(b64))).toEqual(Array.from(original))
  })
})

describe('convertToHtml', () => {
  it('rejects an unsupported extension before touching mammoth/xlsx', async () => {
    await expect(convertToHtml('txt', new Uint8Array(), false)).rejects.toThrow('No viewer for .txt')
  })

  it('the error message includes the leading dot, matching the Electron original', async () => {
    await expect(convertToHtml('pdf', new Uint8Array(), true)).rejects.toThrow(/^No viewer for \.pdf$/)
  })

  // ---- real conversion branches — see this file's module doc comment for
  // why these were previously believed untestable under vitest ----

  it('xlsx: converts a real in-memory workbook to a table per sheet, prefixed by docCss', async () => {
    const wb = XLSX.utils.book_new()
    XLSX.utils.book_append_sheet(wb, XLSX.utils.aoa_to_sheet([['Name', 'Qty'], ['Widget', 3]]), 'Stock')
    const bytes = new Uint8Array(XLSX.write(wb, { type: 'array', bookType: 'xlsx' }))

    const html = await convertToHtml('xlsx', bytes, false)

    expect(html.startsWith(docCss(false))).toBe(true)
    expect(html).toContain('<h3>Stock</h3>')
    expect(html).toContain('<table')
    expect(html).toContain('Widget')
  })

  it('xlsx: renders one <h3>+<table> pair per sheet, in workbook order', async () => {
    const wb = XLSX.utils.book_new()
    XLSX.utils.book_append_sheet(wb, XLSX.utils.aoa_to_sheet([['a']]), 'First')
    XLSX.utils.book_append_sheet(wb, XLSX.utils.aoa_to_sheet([['b']]), 'Second')
    const bytes = new Uint8Array(XLSX.write(wb, { type: 'array', bookType: 'xlsx' }))

    const html = await convertToHtml('xlsx', bytes, true)

    expect(html.indexOf('First')).toBeGreaterThan(-1)
    expect(html.indexOf('First')).toBeLessThan(html.indexOf('Second'))
    expect(html.match(/<h3>/g)).toHaveLength(2)
  })

  it('xlsx: escapes a sheet name containing & and <, matching escapeHtml', async () => {
    const wb = XLSX.utils.book_new()
    XLSX.utils.book_append_sheet(wb, XLSX.utils.aoa_to_sheet([['x']]), 'A&B')
    const bytes = new Uint8Array(XLSX.write(wb, { type: 'array', bookType: 'xlsx' }))

    const html = await convertToHtml('xlsx', bytes, false)

    expect(html).toContain('<h3>A&amp;B</h3>')
  })

  it('docx: converts a real fixture file to its expected paragraph HTML, prefixed by docCss', async () => {
    const bytes = new Uint8Array(fixture('single-paragraph.docx'))

    const html = await convertToHtml('docx', bytes, false)

    expect(html).toBe(docCss(false) + '<p>Walking on imported air</p>')
  })

  it('docx: dark flag threads through to the prefixed docCss block', async () => {
    const bytes = new Uint8Array(fixture('single-paragraph.docx'))

    const html = await convertToHtml('docx', bytes, true)

    expect(html.startsWith(docCss(true))).toBe(true)
  })
})
