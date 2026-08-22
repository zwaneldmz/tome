// Pure spawn-policy predicate shared by the ＋ menu (menus.js) and pane
// spawning (panes.js) — DOM-free so vitest can pin the containment-only
// rules without jsdom (none is set up in this repo).
//
// `containment-only` is a CEILING, not a default. `egress-default`
// ("Spawn agents contained") decides how the NEXT agent pane spawns and the
// user can flip it per pane; `containment-only` removes unsandboxed spawns
// entirely — the menu drops them, the assistant stops proposing them, and
// the backend refuses them at the IPC layer (src-tauri/src/ipc/pty.rs —
// the renderer is a threat-model actor, so the real wall is there, never
// here). This module only controls what the renderer OFFERS.
export function spawnPolicy(prefs) {
  const containmentOnly = !!prefs?.containmentOnly
  return {
    containmentOnly,
    // Agent panes are always gapped under containment-only: the ceiling
    // overrides the egress-default DEFAULT (which stays off/on for the day
    // the ceiling is lifted again).
    agentsGapped: containmentOnly || !!prefs?.egressDefault,
    // The egress-default toggle is a default, not a ceiling — it stops
    // meaning anything under containment-only (agents are gapped
    // regardless), so the ＋ menu hides it rather than show a lying switch.
    showEgressDefaultToggle: !containmentOnly,
    // The plain Terminal row is THE unsandboxed spawn entry (no egress by
    // definition); containment-only removes it from the ＋ menu. The
    // backend refuses the spawn even if some path asks anyway.
    showUnsandboxedTerminal: !containmentOnly,
  }
}
