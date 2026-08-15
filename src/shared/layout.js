// Layout-store shape guards, shared by the renderer and the test suite.
//
// dockview's toJSON() serializes `panels` as an OBJECT keyed by panel id
// ({ [id]: { contentComponent, id, params, title } }), NOT an array. A previous
// guard checked `Array.isArray(saved.panels)` and therefore returned early on
// every boot — layout restore was a silent no-op. Keep the real shape in one
// DOM-free function so a regression is caught by `npm test`.

export function isValidSavedLayout(saved) {
  if (!saved || typeof saved !== 'object') return false
  if (!saved.panels || typeof saved.panels !== 'object' || Array.isArray(saved.panels)) return false
  return Object.keys(saved.panels).length > 0
}
