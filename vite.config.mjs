import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'

// Plain Vite config that drives the Tauri build. Parallel to (and
// independent of) electron.vite.config.mjs, which still drives the Electron
// build during the coexistence period (Phase 7 removes it). src-tauri's
// tauri.conf.json build.devUrl/frontendDist point at this config's dev
// server and build output respectively.
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
    rollupOptions: {
      // popout.html is the shell document a dragged-out pane gets its own OS
      // window for — mirrors electron.vite.config.mjs's renderer inputs.
      input: {
        index: here('src/renderer/index.html'),
        popout: here('src/renderer/popout.html'),
      },
    },
  },
})
