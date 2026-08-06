// Tome app icon: a 16×16 "cyber-grimoire" sprite rendered to every macOS icon
// size with nearest-neighbor scaling. Zero deps — hand-rolled PNG writer.
import { deflateSync } from 'node:zlib'
import { mkdirSync, writeFileSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')

// ---- the sprite ------------------------------------------------------------
// . transparent   # hairline border   B tile void      k book cover
// m magenta sigil g magenta dim       c cyan           t cyan dim
// p page          d page shade
const PALETTE = {
  '.': [0, 0, 0, 0],
  '#': [27, 30, 47, 255], // #1b1e2f
  B: [10, 11, 18, 255], // #0a0b12
  k: [42, 16, 36, 255], // #2a1024
  m: [255, 46, 166, 255], // #ff2ea6
  g: [143, 31, 99, 255], // #8f1f63
  c: [0, 229, 255, 255], // #00e5ff
  t: [11, 79, 92, 255], // #0b4f5c
  p: [232, 240, 248, 255], // #e8f0f8
  d: [143, 163, 184, 255], // #8fa3b8
}

const GRID = [
  '.##############.',
  '#BBBBBBBBBBBBBB#',
  '#BBBBBBBBBBBBBB#',
  '#BcgggggggcgpBB#',
  '#BckkkkkkkckdBB#',
  '#BckkmmkkkckpBB#',
  '#BckkmmkkkckdBB#',
  '#BckkkkmmkckpBB#',
  '#BckkkkmmkckdBB#',
  '#BckkkkkkkckpBB#',
  '#BckkkkkkkckdBB#',
  '#BckkkkkkkckpBB#',
  '#BcpppppppcpdBB#',
  '#BBBBBBBBBcBBBB#',
  '#BBBBBBBBBtBBBB#',
  '.##############.',
]

const N = 16
if (GRID.length !== N || GRID.some((r) => r.length !== N)) {
  console.error('grid must be 16×16')
  for (const [i, r] of GRID.entries()) if (r.length !== N) console.error(`row ${i}: ${r.length}`)
  process.exit(1)
}

// ---- minimal PNG writer ----------------------------------------------------
const CRC_TABLE = new Int32Array(256).map((_, n) => {
  let c = n
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
  return c
})
const crc32 = (buf) => {
  let c = -1
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8)
  return (c ^ -1) >>> 0
}
const chunk = (type, data) => {
  const t = Buffer.from(type)
  const len = Buffer.alloc(4)
  len.writeUInt32BE(data.length)
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(Buffer.concat([t, data])))
  return Buffer.concat([len, t, data, crc])
}
function png(size) {
  const f = size / N
  const stride = 1 + size * 4
  const raw = Buffer.alloc(size * stride)
  for (let y = 0; y < size; y++) {
    const row = GRID[Math.floor(y / f)]
    for (let x = 0; x < size; x++) {
      const [r, g, b, a] = PALETTE[row[Math.floor(x / f)]]
      const o = y * stride + 1 + x * 4
      raw[o] = r
      raw[o + 1] = g
      raw[o + 2] = b
      raw[o + 3] = a
    }
  }
  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(size, 0)
  ihdr.writeUInt32BE(size, 4)
  ihdr[8] = 8 // bit depth
  ihdr[9] = 6 // RGBA
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ])
}

// ---- emit ------------------------------------------------------------------
const iconset = join(root, 'build', 'icon.iconset')
mkdirSync(iconset, { recursive: true })
mkdirSync(join(root, 'docs'), { recursive: true })

const entries = [
  ['icon_16x16.png', 16],
  ['icon_16x16@2x.png', 32],
  ['icon_32x32.png', 32],
  ['icon_32x32@2x.png', 64],
  ['icon_128x128.png', 128],
  ['icon_128x128@2x.png', 256],
  ['icon_256x256.png', 256],
  ['icon_256x256@2x.png', 512],
  ['icon_512x512.png', 512],
  ['icon_512x512@2x.png', 1024],
]
for (const [name, size] of entries) writeFileSync(join(iconset, name), png(size))
writeFileSync(join(root, 'build', 'icon.png'), png(1024))
writeFileSync(join(root, 'docs', 'icon.png'), png(256))
execFileSync('iconutil', ['-c', 'icns', iconset, '-o', join(root, 'build', 'icon.icns')])
console.log('icon.icns + icon.png generated')
