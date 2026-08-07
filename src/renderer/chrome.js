// Window chrome that isn't a menu: the sidebar minimizer and the appearance
// picker. Both persist through the ui-state store.
import { tome } from './util.js'
import { floatingMenu, menuItem, menuLabel } from './menus.js'
import { THEME_GLYPH, THEME_ORDER, onTheme, setTheme, themeState } from './theme.js'

// ---------- left pane minimizer ----------
const sidebarBtn = document.getElementById('btn-sidebar')
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

sidebarBtn.addEventListener('click', (e) => {
  e.stopPropagation()
  setCollapsed(!collapsed)
})
window.addEventListener('keydown', (e) => {
  if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === 'b') {
    e.preventDefault()
    setCollapsed(!collapsed)
  }
})

// ---------- appearance ----------
const themeBtn = document.getElementById('btn-theme')
const THEME_LABEL = { system: 'Match system', light: 'Light', dark: 'Dark' }

themeBtn.addEventListener('click', (e) => {
  e.stopPropagation()
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
})

onTheme(() => {
  themeBtn.textContent = THEME_GLYPH[themeState.pref]
  themeBtn.title = `Appearance — ${THEME_LABEL[themeState.pref]}`
})

export async function bootChrome() {
  if (await tome.store.get('sidebar-collapsed')) setCollapsed(true, false)
}
