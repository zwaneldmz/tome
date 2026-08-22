// Settings navigation — pure, no DOM, no imports. The seven-group table
// the Preferences rail (preferencesModal, slice 3a) and its live search
// are built on; kept module-pure so it gets a vitest suite like every
// other pure module in this repo.
//
// Groups are stable ids ('general', 'assistant', …); sections are the
// data-section ids of the .prefs-section elements the pane renders.

export const GROUPS = [
  { id: 'general', label: 'General', sections: ['appearance', 'terminal', 'editor', 'sidebar'] },
  { id: 'assistant', label: 'Assistant', sections: ['assistant', 'custom-provider'] },
  { id: 'agents', label: 'Agents', sections: ['agents'] },
  { id: 'security', label: 'Security', sections: ['security'] },
  { id: 'integrations', label: 'Integrations', sections: ['export', 'schedules', 'remote'] },
  { id: 'voice', label: 'Voice', sections: ['voice'] },
  { id: 'mentor', label: 'Mentor', sections: ['mentor'] },
]

const SECTION_TO_GROUP = new Map(GROUPS.flatMap((g) => g.sections.map((s) => [s, g.id])))

// Lowercase, collapse runs of whitespace to one space, trim — both sides
// of every comparison so "2FA" meets "(2FA)…" and line-wrapped labels
// match single-space queries.
export function normalize(text) {
  return String(text ?? '')
    .toLowerCase()
    .replace(/\s+/g, ' ')
    .trim()
}

// Filter an index of entries ({groupId, sectionId, text}) down to what a
// query hits. Row-level entries decide row visibility at the call site;
// the returned sets decide section visibility, rail dimming, and the
// match count. An empty (or whitespace-only) query matches everything.
export function filterRows(query, entries) {
  const q = normalize(query)
  const sections = new Set()
  const groups = new Set()
  for (const e of entries || []) {
    if (q && !normalize(e.text).includes(q)) continue
    sections.add(e.sectionId)
    groups.add(e.groupId)
  }
  return { sections, groups, count: sections.size }
}

// The group a section id belongs to, or null for unknown ids.
export function sectionToGroup(sectionId) {
  return SECTION_TO_GROUP.get(sectionId) ?? null
}
