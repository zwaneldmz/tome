# Feature Plan: File/Folder Creation + Agentic Workflow Diagrams ("Flows")

**Repo:** Tome — an Electron desktop "coding harness": agents, terminals, editors, and chat in one bound workspace.
**Stack:** Electron (main / preload / renderer), electron-vite, dockview-core (pane grid), CodeMirror 6, xterm.js, node-pty. Plain ES modules, no framework, no TypeScript.
**Audience:** another coding CLI. Follow the conventions below exactly — the codebase is small, consistent, and commented with *why*, not *what*. Match that.

---

## 0. Architecture primer (read before touching anything)

### Process layout
- `src/main/index.js` (~1110 lines) — all IPC handlers, pty spawning, git, store, dialogs. File-system handlers live here.
- `src/preload/index.js` — `contextBridge.exposeInMainWorld('tome', …)`. The renderer only ever talks to main through `window.tome.*` (imported as `tome` from `src/renderer/util.js`). Every new IPC channel must be added here.
- `src/renderer/` — the whole UI. Entry is `renderer.js`; markup shell is `index.html`.
- `src/shared/pane-kinds.js` — single source of truth for pane kinds, imported by both main and renderer. **If you add a pane kind, it goes here.**

### Key renderer modules
- `src/renderer/panes.js` — the dockview grid. `createComponent` switches on `opts.name` to instantiate panel classes (`EditorPanel`, `ChatPanel`, `TerminalPanel`, `BrainPanel`, `HistoryPanel`, `DocPanel`). Also owns `openFile()`, layout persistence (`dock.toJSON()` per workspace, `componentOf()` infers a panel's type from its persisted `params` on restore), and the `conductor:open` bridge.
- `src/renderer/tree.js` — the file-tree sidebar. Renders into `#tree-body`. `renderDir(dir, container, depth, rootPath)` lazily expands folders; `renderTree()` renders workspace root heads. Junk dirs (`node_modules`, `dist`, …) are dimmed via the `.junk` class.
- `src/renderer/menus.js` — dropdown plumbing (`wireMenu`, `floatingMenu`, `menuItem`, `menuInput`, `menuRule`, `menuLabel`), the workspace menu, and `populateAddMenu()` (the ＋ "new pane" menu, shared by the topbar ＋ and each pane-group header ＋).
- `src/renderer/modals.js` — `promptModal(title, placeholder, initial, submitLabel)` → resolves string|null; `confirmModal(title, note, label)` → boolean; `choiceModal(...)`; all built on `modalShell` (focus-trapped, Escape/scrim cancel, one at a time, `doc` param for popout windows).
- `src/renderer/util.js` — `el(tag, cls, text)` DOM helper, `toast(msg, kind)`, `tome` bridge.
- `src/renderer/icons.js` — `folderIcon(open)`, `fileIcon()`, etc. Add new SVG icons here.
- `src/renderer/state.js` / `workspaces.js` — `wsState` (workspaces, `activeRoot`), `prefs`, `counters.seq` (pane id sequence), `activeWorkspace()`, `saveWs()`.
- `src/renderer/statusbar.js` / `git.js` — refreshed by `renderAll()` in menus.js.

### Conventions that matter
- **IPC naming:** `domain:verb` (e.g. `fs:readDir`, `dialog:pickFolder`). Renderer calls via `tome.<domain>.<verb>()`.
- **Panel contract:** a panel class has `element` (DOM node) and `init({ params, api })`; optional `isDirty()`, `dispose()`. Panels are added via `dock.addPanel({ id, component, title, position, params })`. The `params` object is what survives layout persistence — `componentOf()` in panes.js must be able to re-derive the component from it, and `restoreLayout()` must respawn it.
- **Security posture (do not regress):** `src/main/index.js` has a *file-open confinement* system (`isConfinedPath`, `confinedRealPath`) that vets paths parsed in main on behalf of the *model* (conductor tools, `doc:read`) against open workspace folders + brain vaults. `fs:readFile`/`fs:writeFile` are deliberately unvetted (user-driven; see the comment at the top of index.js). New write-capable IPC that can be triggered by the **assistant/conductor** must go through confinement; user-driven tree actions follow the existing `fs:writeFile` precedent.
- **Comments:** explain *why*, including bugs a naive approach would hit. See panes.js for the house style.
- **Tests:** vitest, in `test/`. Main-process logic (conductor, brain, airgap, authlock) has unit tests; renderer DOM code does not.
- **No new heavy dependencies without justification.** The flows canvas should be hand-rolled (SVG/DOM) — see §3.4.

---

## 1. Feature 1 — Create folder / create file buttons

### 1.1 IPC (main + preload)

`fs:writeFile` already exists (`src/main/index.js:651`) but fails if the parent dir doesn't exist, and there is no mkdir channel.

1. **`src/main/index.js`** — add two handlers next to the existing `fs:*` block (~line 643–660):
   ```js
   ipcMain.handle('fs:mkdir', (e, p) => mkdir(p, { recursive: true }))
   ```
   (`mkdir` is already imported from `node:fs/promises` at the top of the file.)
   For file creation, reuse `fs:writeFile` but add an exclusive-create variant so a name collision can't silently clobber:
   ```js
   ipcMain.handle('fs:createFile', async (e, p) => {
     // 'wx' — fail rather than overwrite an existing file
     await writeFile(p, '', { flag: 'wx' })
   })
   ```
2. **`src/preload/index.js`** — extend the `fs:` block:
   ```js
   mkdir: (p) => ipcRenderer.invoke('fs:mkdir', p),
   createFile: (p) => ipcRenderer.invoke('fs:createFile', p),
   ```

### 1.2 Tree UI (`src/renderer/tree.js`)

The tree header (`#tree-head` in `index.html`) currently holds only the sidebar toggle. Add two icon buttons there: **New file** and **New folder**. They create entries in `wsState.activeRoot` (the currently-activated root folder — the tree already tracks this via `setActiveRoot`).

Behavior spec:
- Click → `promptModal('New file', 'name or path — e.g. src/util.js')` (modals.js already exists; validate: reject absolute paths and `..` segments, since the path is joined onto the root).
- Resolve target: `` `${wsState.activeRoot}/${input}` ``. For files, create parent dirs implicitly (`tome.fs.mkdir` on the dirname first, then `createFile`). For folders, `mkdir` full input path.
- On success: `renderTree()` (full re-render is what every other mutation in this app does — see `renderAll` in menus.js), then for files call `openFile(fullPath)` from panes.js so the new file opens in an editor. On `EEXIST`: `toast(`“${name}” already exists`)`.
- Disabled state: if there's no active workspace/root, disable the buttons (title: "needs a workspace folder").

Also add the same two actions to the per-root context affordance: the root-head row (`renderTree()` in tree.js) already has a `×` remove button; add a small `＋` button on the root head that opens a `choiceModal`-style mini menu (or reuse `floatingMenu` from menus.js) with "New file here" / "New folder here", targeting that root instead of `activeRoot`. Keep it simple: if this gets fiddly, the header buttons scoped to `activeRoot` are the MVP.

**Refresh caveat:** `renderDir` expansion state is local (`open`/`kids` closures) and lost on `renderTree()`. That's acceptable (matches the existing remove-folder behavior), but note it in a comment.

**Icons:** add `newFileIcon()` / `newFolderIcon()` to `src/renderer/icons.js` (follow the existing inline-SVG style), and style the buttons in `style.css` next to `#tree-head` rules.

### 1.3 Also wire into the ＋ menu (`menus.js` → `populateAddMenu`)

Add below "Open file…":
```js
menuItem(menu, { label: 'New file…', disabled: !wsState.activeRoot, onClick: createFileInActiveRoot })
menuItem(menu, { label: 'New folder…', disabled: !wsState.activeRoot, onClick: createFolderInActiveRoot })
```
Put the shared create logic in tree.js and export it (menus.js already imports `renderTree` from tree.js — same import cycle pattern, noted there as safe).

### 1.4 Tests
- `test/` is main-process only; add a small test for path validation if you extract it into a pure helper (e.g. `src/renderer/tree-create.js` exporting `validateRelPath(input)` → `{ ok, reason }`). Vitest can import it directly since it's a pure function.

---

## 2. Feature 2 — "Flows": agentic workflow diagrams

### 2.1 Concept (align with the user's ask)

A **Flow** is a directed graph of **agent nodes**. Each node is an agent with:
- `name` and `kind` (`'claude' | 'opencode' | 'pi' | 'terminal'` — reuse `AGENTS` from `src/shared/pane-kinds.js`),
- `instructions` — what this agent does / its system-prompt-style brief,
- `expects` — what it needs from upstream (preceding) nodes: freeform text + named **inputs**,
- `produces` — what downstream nodes can require from it: named **outputs** with a description.

**Edges** connect an output of node A to an input of node B — that *is* the contract surface the user described ("requirements and instructions on what to either expect from another node or what is needed from proceeding nodes"). The edge label is the mapping `A.output → B.input`.

Flows are saved as JSON files on disk (see §2.3) so they live in the user's workspace, are diffable in git, and open in the existing editor as raw JSON if wanted.

Execution (MVP): **materialize the flow into panes.** "Run" topologically sorts the graph, spawns one agent terminal pane per node (existing `spawnTerminal` machinery, which already handles air-gap defaults and layout persistence), and pastes a composed bootstrap prompt into each agent: its instructions + its `expects` + the contracts of its incoming edges. Actual command submission still respects the existing `conductor:allowRun` / air-gap gates — do not bypass them. Inter-node message passing beyond bootstrap is **out of scope for v1** (see §4).

### 2.2 Data model

```jsonc
// <name>.flow.json
{
  "version": 1,
  "name": "review-pipeline",
  "nodes": [
    {
      "id": "n1",
      "kind": "claude",            // AGENTS member or 'terminal'
      "name": "Researcher",
      "instructions": "Survey the codebase for auth code…",
      "expects": "A ticket description from the user.",
      "produces": "A findings report: file list + risk notes.",
      "inputs": [],                // [{ name, description }]
      "outputs": [{ "name": "report", "description": "findings markdown" }],
      "x": 120, "y": 80            // canvas position
    }
  ],
  "edges": [
    { "id": "e1", "from": "n1", "to": "n2",
      "fromOutput": "report", "toInput": "findings",
      "label": "findings report" }
  ]
}
```

Validation rules (enforce in a pure module so it's testable):
- node ids unique; edge endpoints reference existing nodes; `fromOutput` ∈ source node's outputs, `toInput` ∈ target's inputs (warn, don't hard-fail, on load of hand-edited files);
- cycles allowed in the model but "Run" refuses cyclic graphs (topo sort fails → toast).

### 2.3 Where files live

- Flow files: `<activeRoot>/.tome/flows/<name>.flow.json` (create dirs on demand with the new `fs:mkdir`). `.tome/` should be added to `JUNK_DIRS` in tree.js? **No** — flows are user content; instead leave it visible. (Decide: if the team prefers hidden, add to JUNK_DIRS.)
- "New flow" action: ＋ menu entry `Flow diagram` + a flow node in the tree is just a file; double-clicking a `.flow.json` opens the **Flow panel** (not the text editor) — hook into `openFile()` in panes.js: check `path.endsWith('.flow.json')` before the text/binary sniff, route to the flow panel. Add an "open as text" escape hatch in the flow panel toolbar.

### 2.4 The Flow panel (new pane kind)

**Why hand-rolled:** the app's CSP (`index.html`) is tight (`script-src 'self'`), deps are deliberately minimal, and the interaction surface (drag nodes, draw edges, edit fields) is ~400 lines of SVG/DOM. Do **not** add React Flow / dagre / elkjs. If auto-layout is wanted later, write a 30-line layered-topo layout.

Files:
- `src/renderer/panels/flow.js` — `FlowPanel` class following the panel contract (model after `panels/brain.js`, the richest existing panel: toolbar + body + modal-based editing).
- `src/renderer/flow-model.js` — pure functions: `createFlow()`, `addNode/edge`, `validateFlow()`, `topoSort()`, `composeBootstrapPrompt(node, incomingEdges)`. Unit-testable.
- `src/shared/pane-kinds.js` — add `'flow'` to `OPENABLE_KINDS` if the conductor should open flows (recommended: yes, it's how the assistant can show a flow it wrote).

Rendering approach:
- One absolutely-positioned `<div class="flow-node">` per node inside a relatively-positioned canvas div; **edges as a single `<svg>` under the nodes**, paths recomputed on drag (cubic bezier between the output port and input port anchors).
- Drag: pointer events on the node header; update `x/y`, recompute edge paths. Pan: drag on empty canvas (translate a content wrapper). Zoom: optional for v1 — skip unless trivial (CSS transform scale + pointer delta correction is the classic bug farm; comment why if skipped).
- Node body shows: kind badge, name, truncated instructions, input ports (left) and output ports (right) as small dots with labels. Edge drawing: pointerdown on an output port → temp path follows cursor → pointerup on an input port creates the edge (validate: no self-loop, no duplicate edge on the same port pair).
- Edit node: click (not drag) opens a `modalShell`-based editor with fields for name, kind (select over `AGENTS`), instructions (textarea), expects, produces, and add/remove input/output port rows. Reuse `modals.js` patterns; it already handles focus trap + Escape.
- Toolbar (panel header row, see brain.js for style): flow name, **Save** (dirty tracking like EditorPanel: `isDirty()`, ● title prefix, close guard comes free via panes.js), **Run** (§2.5), **Open as text**, **Export PNG** optional (skip v1).
- Delete: node/edge selected → Delete key + a small ✕ on selection; confirm only if node has edges (`confirmModal`).

Persistence & restore:
- `component: 'flow'`, `params: { path }` — extend `componentOf()` in panes.js (`if (params.path?.endsWith('.flow.json')) return 'flow'` — order matters, put it before the generic `params.path → 'editor'` fallthrough) and the `restoreLayout()` switch (reuse `fileExists`).
- Saving writes JSON via existing `tome.fs.writeFile`.
- Register in `createComponent` switch in panes.js: `case 'flow': return new FlowPanel()`.
- Layout restore of terminals spawned by a run already works (they're plain terminal panes).

Styling: `style.css` — follow existing custom-property theme tokens (look at `.panel-editor`, `.watermark`, menu styles). Must work in both light/dark (theme.js swaps a class; use the CSS vars, never hard-coded colors) and in popout windows (dockview copies stylesheets; avoid `document` globals — panels get their own element, and any modal needs the `doc` param when opened from a popout: `modalShell(…, this.element.ownerDocument)`).

### 2.5 Run a flow

In `FlowPanel`:
1. `topoSort()` — on cycle, `toast('flow has a cycle — cannot run')`.
2. For each node in topo order: `spawnTerminal({ kind: node.kind, cwd: wsState.activeRoot, target: { group } })` — spawn the first normally, subsequent ones as tabs into the first's group so a run stays stacked (mirrors how conductor-opened panes join the source group; see `groupTarget` in panes.js). Respect the pane's air-gap default exactly as `addTerminal` does.
3. Compose the bootstrap prompt in `flow-model.js`:
   ```
   You are "<name>" in a Tome flow "<flowName>".
   Instructions: <instructions>
   You receive: <expects + for each incoming edge: '"<fromOutput>" from <fromNode>: <edge.label>'>
   You must produce: <produces + output names>
   Hand off by writing your outputs to .tome/flows/<flowName>/<nodeId>-<output>.md and telling the user when done.
   ```
   (File-based handoff is deliberate: it works with the existing unmodified agent CLIs, is inspectable, and requires no new IPC. Downstream nodes' prompts reference those paths.)
4. Deliver the prompt via the **existing conductor path**: `tome.conductor` only exposes `type_in_terminal` to the model, not the renderer — so instead write directly to the pty: the terminal panel owns its pty id (`params.ptyId`); expose a small helper in panes.js/terminal.js `typeIntoPanel(panel, text)` that calls `tome.pty.write(ptyId, text)` **without** pressing enter (same semantics as conductor with auto-run off — the user reviews and submits). Never auto-submit from flows v1; comment why (mirrors the allowRun gate).

### 2.6 Conductor integration (optional but cheap, do it)

- Add `'flow'` to `OPENABLE_KINDS` in `src/shared/pane-kinds.js` and a `case` in the `tome.conductor.onOpen` switch in panes.js (`addFlow(target)` → new untitled flow panel, or `openFile` when `file` is given — the existing `file` branch already covers `.flow.json` once §2.3's openFile hook lands).
- This lets the assistant draft a flow JSON to disk (it can already ask the user to create files) and open it visually. No new main-process tools needed; **do not** give the conductor a raw fs-write tool — confinement (§0) applies and is out of scope.

### 2.7 Tests (`test/flow.test.js`)

Pure-module tests for `flow-model.js`: validateFlow (dupes, dangling edges, bad ports), topoSort (order, cycle detection), composeBootstrapPrompt (includes contracts, lists edge mappings). Follow the style of `test/conductor.test.js`.

---

## 3. Build order (suggested commits)

1. **fs plumbing:** `fs:mkdir` + `fs:createFile` (main, preload) + unit test for path validation helper.
2. **Tree buttons:** header buttons, prompt modals, create + open, ＋ menu entries.
3. **flow-model.js + tests** (no UI).
4. **FlowPanel read-only render:** open a `.flow.json`, draw nodes/edges, pan. Wire `openFile` hook, `componentOf`, restore.
5. **Editing:** node drag, edge draw, node editor modal, delete, save + dirty guard.
6. **Run:** topo sort, spawn, bootstrap prompts via `pty.write`.
7. **Conductor kind + polish:** icons, empty state, docs (`README.md` short section; `docs/` update if there's a natural home).

## 4. Explicit non-goals (v1)

- No live inter-agent messaging / watching handoff files to auto-continue the chain. (Future: `fs:watch` on the flow's handoff dir + a "continue" affordance.)
- No auto-layout, minimap, zoom, PNG export, subflows, conditional edges.
- No new npm dependencies. No TypeScript. No framework.
- No auto-run of agent commands from flows — submission stays user-gated, consistent with the conductor's `allowRun` model.

## 5. Gotchas (learned from the codebase — don't rediscover the hard way)

- **Popout windows:** any modal/menu opened from a panel that can be torn off must target `element.ownerDocument` (see `choiceModal(..., doc)` usage in panes.js and `floaterFor` in menus.js).
- **Layout restore:** a panel whose `componentOf()` returns null is *removed* on restore — forgetting the flow branch silently deletes flow panes on restart.
- **`openFile` ordering:** the `.flow.json` check must precede the text-sniff, or flows open as text.
- **CSP:** `script-src 'self'` — no CDN anything; SVG built via DOM APIs, not `innerHTML` with strings (house style uses `el()` anyway).
- **EEXIST:** use `flag: 'wx'` for create; `mkdir` is fine with `recursive: true` (idempotent).
- **Tree re-render loses expansion** — acceptable, comment it.
- **`counters.seq`** is the pane-id sequence; flow-spawned terminals go through `spawnTerminal`, which handles it — don't mint your own ids.
- **Theme:** only CSS variables; verify in light + dark (`btn-theme` toggles).
