// Dependency-audit CI gate. Reads `bun audit --json` (or `npm audit --json`)
// from stdin and fails (nonzero exit) on any advisory at or above the given
// severity level unless reviews/validation/audit-exceptions.json lists it
// with a reason and an unexpired `expires` date. Neither audit tool's exit
// code has any concept of a reviewed exception, so this script — not the
// audit command — is CI's pass/fail authority; see .github/workflows/build.yml
// for how the two are wired. bun audit's JSON shape differs from npm's, so
// normalizeAudit() folds bun's into npm's before evaluateAudit() runs.
//
// Usage: bun audit --json | node scripts/audit-gate.mjs <level>
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const SEVERITY_RANK = { info: 0, low: 1, moderate: 2, high: 3, critical: 4 }
const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const exceptionsPath = resolve(root, 'reviews/validation/audit-exceptions.json')

function fail(message) {
  process.stderr.write(`audit-gate: ${message}\n`)
  process.exitCode = 1
}

// Reads fd 0 to EOF in one call — Node resolves readFileSync(0) as a
// blocking full read even on a pipe, no streaming/event loop needed for a
// bounded JSON payload like an audit report.
function readStdin() {
  try {
    return readFileSync(0, 'utf8')
  } catch (error) {
    throw new Error(`could not read stdin: ${error.message}`)
  }
}

// Exceptions file is advisory id -> { reason, expires }. Missing file fails
// safe (closed): zero exceptions applied, not zero enforcement.
function loadExceptions() {
  let raw
  try {
    raw = readFileSync(exceptionsPath, 'utf8')
  } catch {
    process.stderr.write(`audit-gate: no exceptions file at ${exceptionsPath} — treating as empty\n`)
    return {}
  }
  const parsed = JSON.parse(raw)
  if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
    throw new Error(`${exceptionsPath} must be a JSON object of advisory id -> {reason, expires}`)
  }
  return parsed
}

export function isLive(entry) {
  if (!entry || typeof entry.reason !== 'string' || !entry.reason.trim()) return false
  if (typeof entry.expires !== 'string') return false
  const expiresAt = Date.parse(entry.expires)
  return Number.isFinite(expiresAt) && Date.now() < expiresAt
}

// GHSA/advisory identity from a `via` entry: prefer the GHSA slug from the
// advisory URL (what humans cite), fall back to npm's numeric source id.
export function advisoryId(via) {
  const match = typeof via.url === 'string' && via.url.match(/advisories\/([\w-]+)/)
  if (match) return match[1]
  if (via.source !== undefined) return String(via.source)
  return null
}

// The blocked/excepted decision itself: given npm audit's `vulnerabilities`
// object, the loaded exceptions map, and a severity threshold, returns which
// packages are blocked (unexcepted advisories at/above threshold) plus
// reviewed/excepted counts. No I/O (no stdin, no file read) — CI's actual
// pass/fail authority is a pure function of its inputs, testable offline.
export function evaluateAudit(vulnerabilities, exceptions, threshold) {
  const blocked = []
  let excepted = 0
  let reviewed = 0
  for (const vuln of Object.values(vulnerabilities)) {
    const rank = SEVERITY_RANK[vuln.severity] ?? SEVERITY_RANK.critical // unknown severity fails safe (max)
    if (rank < threshold) continue
    reviewed++

    const advisories = (vuln.via || []).filter((via) => typeof via === 'object')
    const unexcepted = []
    for (const via of advisories) {
      const id = advisoryId(via)
      const entry = id ? exceptions[id] : undefined
      if (id && isLive(entry)) {
        excepted++
      } else {
        unexcepted.push({ id: id || '(unknown)', title: via.title, url: via.url })
      }
    }
    if (unexcepted.length > 0) blocked.push({ name: vuln.name, severity: vuln.severity, unexcepted })
  }
  return { blocked, excepted, reviewed }
}

// bun audit --json emits a bare map { "<package>": [ <advisory>, ... ] } where
// each advisory carries `severity` directly; npm audit nests it as
// { vulnerabilities: { "<package>": { severity, via: [<advisory>...] } } }.
// Fold bun's shape into npm's so evaluateAudit() stays the single pass/fail
// authority for both. The package's severity is the worst advisory in its
// array (npm reports the max the same way), and `via` carries { title, url,
// source } so advisoryId() resolves GHSA slugs and numeric ids unchanged.
export function bunToNpm(report) {
  const vulnerabilities = {}
  for (const [name, advisories] of Object.entries(report || {})) {
    if (!Array.isArray(advisories)) continue
    const ranks = advisories.map((a) => SEVERITY_RANK[a.severity] ?? SEVERITY_RANK.critical)
    const maxRank = ranks.length ? Math.max(...ranks) : SEVERITY_RANK.critical
    const severity = Object.keys(SEVERITY_RANK).find((k) => SEVERITY_RANK[k] === maxRank) || 'critical'
    vulnerabilities[name] = {
      name,
      severity,
      via: advisories.map((a) => ({ title: a.title, url: a.url, source: a.id })),
    }
  }
  return { vulnerabilities }
}

// npm audit's JSON has a top-level `vulnerabilities` object; bun audit's has
// a bare name->advisories map. Pass npm through untouched, transform bun.
export function normalizeAudit(report) {
  if (report && report.vulnerabilities && typeof report.vulnerabilities === 'object') return report
  return bunToNpm(report)
}

function main() {
  const level = process.argv[2]
  if (!level || !(level in SEVERITY_RANK)) {
    fail(`usage: node scripts/audit-gate.mjs <${Object.keys(SEVERITY_RANK).join('|')}>`)
    return
  }
  const threshold = SEVERITY_RANK[level]

  let report
  try {
    report = JSON.parse(readStdin())
  } catch (error) {
    fail(`could not parse audit output as JSON: ${error.message}`)
    return
  }
  if (report.error) {
    fail(`audit did not complete: ${report.error.summary || report.error.code || 'unknown error'}`)
    return
  }
  report = normalizeAudit(report)
  if (!report.vulnerabilities || typeof report.vulnerabilities !== 'object') {
    fail('audit output has no "vulnerabilities" object — treating as a failed audit run, not a clean one')
    return
  }

  let exceptions
  try {
    exceptions = loadExceptions()
  } catch (error) {
    fail(error.message)
    return
  }

  const { blocked, excepted, reviewed } = evaluateAudit(report.vulnerabilities, exceptions, threshold)

  if (blocked.length > 0) {
    process.stderr.write(`audit-gate: BLOCKED — unexcepted advisories at/above "${level}"\n`)
    for (const pkg of blocked) {
      process.stderr.write(`  ${pkg.name} (${pkg.severity}):\n`)
      for (const advisory of pkg.unexcepted) {
        process.stderr.write(`    ${advisory.id} — ${advisory.title || 'no title'}\n`)
        if (advisory.url) process.stderr.write(`    ${advisory.url}\n`)
      }
    }
    process.stderr.write(
      `  Add a reviewed entry to reviews/validation/audit-exceptions.json (with a reason and an\n` +
        `  unexpired "expires" date) or fix the dependency to clear this gate.\n`,
    )
    process.exitCode = 1
    return
  }

  process.stdout.write(
    `audit-gate: OK — ${reviewed} advisory package(s) at/above "${level}", ${excepted} covered by a live exception, 0 unexcepted\n`,
  )
}

// Guarded so this module can be imported for its exported pure functions
// (test/audit-gate.test.js) without running the CLI's stdin/exit-code side
// effects — `node scripts/audit-gate.mjs <level>` still runs main() exactly
// as before, since that invocation is always the entry module.
if (import.meta.main) main()
