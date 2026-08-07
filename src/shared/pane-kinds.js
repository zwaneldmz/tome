// Single source of truth for pane kinds. Imported by main (AGENTS vetting,
// agents:list), the conductor tool description, and the renderer's
// conductor:open switch — adding an agent touches this file only.
// electron-vite bundles both main and renderer, so this import graph works
// across processes; the preload never needs it (it speaks channel names).

// Agent CLIs spawnable as panes — the pty command line is built in main
// from these vetted names, never from renderer-supplied binaries/args.
export const AGENTS = ['claude', 'opencode', 'pi']

// Kinds the conductor's open_pane tool may ask for; anything else gets
// toasted as unknown by the renderer.
export const OPENABLE_KINDS = ['terminal', ...AGENTS, 'chat', 'brain']

export const OPENABLE_KINDS_DESCRIPTION = `kind is one of: ${OPENABLE_KINDS.map((k) => `'${k}'`).join(', ')}.`
