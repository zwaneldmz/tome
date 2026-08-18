// Repo-committed egress allowlist (.tome/egress.json): detection and consent.
// The file is UNTRUSTED — anyone who can commit to the repo can widen the
// egress of every gapped pane pointed at it — so nothing from it takes
// effect until the user clicks Allow, and the consent is pinned to the file's
// SHA-1: any post-consent edit re-prompts. Consent is COLLECTED here but
// VERIFIED AND STORED in main (userData/egress-repo-consents.json, 0600,
// seatbelt-denied to agents): main re-reads and re-hashes the file at consent
// time and at every boot/workspace-sync, so a compromised renderer can only
// ask main to re-check the file — it cannot widen egress on its own.
import { tome, toast, el } from './util.js'
import { modalShell } from './modals.js'
import { activeWorkspace } from './workspaces.js'

// One check at a time: saveWs and setActiveRoot can both fire during a
// workspace switch, and two concurrent runs could stack two modals for the
// same file.
let running = false

// The consent channel is lock-gated in main, so a stray call while the app
// is locked throws 'Tome is locked.' — that one is expected and swallowed;
// the next unlock-time boot check picks the file up again.
async function consent(root, hash) {
  try {
    const r = await tome.egress.consentRepo(root, hash)
    if (!r.ok) {
      toast(`repo allowlist not applied: ${r.error}`, 'err')
      return
    }
    for (const rej of r.rejected || [])
      toast(`repo allowlist rejected ${JSON.stringify(rej.pattern)}: ${rej.reason}`, 'err')
    if (r.applied?.length)
      toast(`repo allowlist active: ${r.applied.length} hosts from .tome/egress.json`, 'ok')
  } catch (err) {
    if (!String(err?.message).includes('Tome is locked')) throw err
  }
}

function consentModal(root, hosts, rejected) {
  return new Promise((resolve) => {
    const m = modalShell(`⛨ ${root.split('/').pop() || root} asks to widen network access`, () =>
      resolve(false)
    )
    m.note(
      'This repo commits a .tome/egress.json. Anyone who can commit to this repo can edit this list; it stays in force until the file changes.'
    )
    const list = el('ul', 'ag-hosts')
    for (const h of hosts) list.appendChild(el('li', '', h))
    m.body.appendChild(list)
    // Main refuses these even if the user consents — say so up front.
    for (const rej of rejected || [])
      m.note(`rejected: ${JSON.stringify(rej.pattern)} — ${rej.reason}`)
    m.button(`Allow ${hosts.length} hosts`, () => {
      resolve(true) // first resolve wins; the onClose false is a no-op
      m.close()
    })
    m.button('Cancel', () => m.close(), 'ghost')
  })
}

async function checkFolder(root) {
  // Main is the authority: it resolves, reads, hashes, and validates the
  // file, and tells us whether a stored consent already covers it.
  const r = await tome.egress.readRepo(root)
  if (r.state !== 'present' || r.consented) return
  // The modal lists exactly the validated set main would apply.
  if (!(await consentModal(root, r.hosts, r.rejected))) {
    toast('repo egress allowlist ignored', 'err')
    return // store nothing: a cancelled consent must re-ask next time
  }
  await consent(root, r.hash)
}

// Scans the active workspace's folders (each folder consents separately —
// multi-folder workspaces can mix trusted and untrusted repos). Called at
// boot after workspace restore, and on every workspace mutation via saveWs.
export async function checkRepoEgress() {
  if (running) return
  running = true
  try {
    const folders = activeWorkspace()?.folders || []
    for (const folder of folders) await checkFolder(folder)
  } catch (err) {
    // Locked mid-check is fine; anything else is a bug worth a console line.
    if (!String(err?.message).includes('Tome is locked')) console.warn('repo egress check:', err)
  } finally {
    running = false
  }
}
