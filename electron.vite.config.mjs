import { fileURLToPath } from 'node:url'
import { defineConfig, externalizeDepsPlugin } from 'electron-vite'

const here = (p) => fileURLToPath(new URL(p, import.meta.url))

export default defineConfig({
  main: { plugins: [externalizeDepsPlugin()] },
  preload: { plugins: [externalizeDepsPlugin()] },
  renderer: {
    build: {
      rollupOptions: {
        // popout.html is the shell document a dragged-out pane gets its own OS
        // window for — it must ship next to index.html in the renderer output
        input: {
          index: here('src/renderer/index.html'),
          popout: here('src/renderer/popout.html')
        }
      }
    }
  }
})
