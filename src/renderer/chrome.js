// Window chrome that isn't a menu: the sidebar minimizer and the appearance
// picker. Both persist through the ui-state store.
import { tome } from './util.js'
import { floatingMenu, menuItem, menuLabel } from './menus.js'
import { THEME_GLYPH, THEME_ORDER, onTheme, setTheme, themeState } from './theme.js'
import { sidebarIcon, themeIcon, bellIcon, plusIcon, branchIcon } from './icons.js'

// ---------- left pane minimizer ----------
const sidebarBtn = document.getElementById('btn-sidebar')
sidebarBtn.querySelector('.chev').appendChild(sidebarIcon())
let collapsed = false

function setCollapsed(v, animate = true) {
  collapsed = !!v
  // The width transition is armed only around a toggle — left on permanently
  // it would lag the sidebar's native resize handle on every drag frame.
  if (animate) {
    document.body.classList.add('tree-anim')
    setTimeout(() => document.body.classList.remove('tree-anim'), 320)
  }
  document.body.classList.toggle('tree-collapsed', collapsed)
  sidebarBtn.classList.toggle('on', collapsed)
  sidebarBtn.title = collapsed ? 'Show sidebar (⌘B)' : 'Hide sidebar (⌘B)'
  tome.store.set('sidebar-collapsed', collapsed)
}

export const toggleSidebar = () => setCollapsed(!collapsed)

sidebarBtn.addEventListener('click', (e) => {
  e.stopPropagation()
  toggleSidebar()
})
// ⌘B is the native menu bar's 'Toggle Sidebar' accelerator (menu-bridge
// routes it here); no renderer keydown needed anymore.

// ---------- sidebar drag divider ----------
const tree = document.getElementById('tree')
const divider = document.getElementById('tree-divider')
const TREE_MIN = 150
const TREE_MAX = 480

// pointer-based drag-to-resize; the inline width survives the collapse
// (body.tree-collapsed wins with !important), so releasing ⌘B restores it
divider.addEventListener('pointerdown', (e) => {
  if (collapsed) return
  e.preventDefault()
  divider.setPointerCapture(e.pointerId)
  divider.classList.add('dragging')
  const onMove = (e) => {
    const w = Math.min(TREE_MAX, Math.max(TREE_MIN, e.clientX))
    tree.style.width = `${w}px`
  }
  const onUp = () => {
    divider.classList.remove('dragging')
    divider.removeEventListener('pointermove', onMove)
    divider.removeEventListener('pointerup', onUp)
    divider.removeEventListener('pointercancel', onUp)
    tome.store.set('sidebar-width', parseInt(tree.style.width, 10))
  }
  divider.addEventListener('pointermove', onMove)
  divider.addEventListener('pointerup', onUp)
  divider.addEventListener('pointercancel', onUp)
})

// ---------- appearance ----------
const themeBtn = document.getElementById('btn-theme')
const THEME_LABEL = { system: 'Match system', light: 'Light', dark: 'Dark' }

// Shared by the topbar button and (via menu-bridge) the View ▸ Appearance
// submenu, so the native menu can offer the same radio choices.
export function openThemeMenu() {
  floatingMenu(themeBtn, (menu) => {
    menuLabel(menu, 'Appearance')
    for (const pref of THEME_ORDER) {
      menuItem(menu, {
        label: `${THEME_GLYPH[pref]}  ${THEME_LABEL[pref]}`,
        active: themeState.pref === pref,
        onClick: () => setTheme(pref),
      })
    }
  })
}

themeBtn.addEventListener('click', (e) => {
  e.stopPropagation()
  openThemeMenu()
})

onTheme(() => {
  themeBtn.replaceChildren(themeIcon(themeState.pref))
  themeBtn.title = `Appearance — ${THEME_LABEL[themeState.pref]}`
})

// ---------- static topbar icons (bell, add, git glyph) ----------
document.getElementById('btn-notifs').appendChild(bellIcon())
document.getElementById('btn-add').querySelector('.plus').appendChild(plusIcon())
document.getElementById('git-chip').querySelector('.gly').appendChild(branchIcon())

export async function bootChrome() {
  const w = await tome.store.get('sidebar-width')
  if (typeof w === 'number' && w >= TREE_MIN && w <= TREE_MAX) tree.style.width = `${w}px`
  if (await tome.store.get('sidebar-collapsed')) setCollapsed(true, false)
}
