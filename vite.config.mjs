import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'

// Plain Vite config that drives the Tauri build — the only frontend build
// since the Electron removal. src-tauri's tauri.conf.json
// build.devUrl/frontendDist point at this config's dev server and build
// output respectively.
//
// Deliberately NOT read by `npx vitest run`: vitest.config.mjs is a
// dedicated Vitest config, so Vitest resolves against it alone and never
// merges or falls back to this file.
const here = (p) => fileURLToPath(new URL(p, import.meta.url))

export default defineConfig({
  root: here('src/renderer'),
  base: './',
  build: {
    // Absolute path, so it lands at <repo>/dist-web regardless of `root`
    // above being a subdirectory (Vite does not re-relativize an absolute
    // outDir against root).
    outDir: here('dist-web'),
    // outDir resolves outside of root, so Vite would otherwise warn and
    // leave it un-emptied by default — this app wants a clean rebuild.
    emptyOutDir: true,
    // Vite 8's default browser target (Baseline: Chrome 111 / Safari 16.4)
    // is newer than the WebKitGTK / WKWebView floor Tauri v2 still supports
    // on older Linux and macOS hosts — pin to Tauri's recommended floor.
    target: ['es2021', 'chrome105', 'safari15'],
    rolldownOptions: {
      // popout.html is the shell document a dragged-out pane gets its own OS
      // window for — both documents are built from the same renderer tree.
      input: {
        index: here('src/renderer/index.html'),
        popout: here('src/renderer/popout.html'),
      },
    },
  },
})
