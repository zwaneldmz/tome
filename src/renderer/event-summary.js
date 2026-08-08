// Pure display helpers for the event log pane — extracted from
// panels/events.js so the summary/stamp formatting is testable without a
// DOM. The log holds kinds + identifiers only (never payloads), so the
// summary is safe to show verbatim.

// Short relative stamp: "14:02" today, "Tue 14:02" this week, "Aug 2" older.
export function stamp(ts) {
  const d = new Date(ts)
  if (isNaN(d)) return ''
  const now = new Date()
  const hm = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  const days = Math.floor((now.setHours(0, 0, 0, 0) - new Date(d).setHours(0, 0, 0, 0)) / 86_400_000)
  if (days <= 0) return hm
  if (days < 7) return d.toLocaleDateString([], { weekday: 'short' }) + ' ' + hm
  return d.toLocaleDateString([], { month: 'short', day: 'numeric' })
}

// One-line summary of the identifying fields. `count` comes from main's
// blocked-event coalescing: "× N" means N attempts in the coalesce window.
export function summary(rec) {
  switch (rec.kind) {
    case 'conductor:tool':
      return [rec.tool, rec.hint, rec.ok === false ? 'failed' : ''].filter(Boolean).join(' · ')
    case 'airgap:unlock':
      return [rec.paneId, rec.minutes != null ? `${rec.minutes}m` : ''].filter(Boolean).join(' · ')
    case 'airgap:relock':
      return rec.paneId || ''
    case 'airgap:blocked':
      return [
        rec.host,
        rec.paneId,
        rec.count > 1 ? `× ${rec.count}` : '',
      ]
        .filter(Boolean)
        .join(' · ')
    default:
      return Object.entries(rec)
        .filter(([k]) => k !== 'ts' && k !== 'kind')
        .map(([k, v]) => String(v))
        .join(' · ')
  }
}
