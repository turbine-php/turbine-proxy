import { describe, it, expect, beforeEach } from 'vitest'

// ─── Token helpers (extracted inline for unit testing) ────────────────────────
const TOKEN_KEY = 'turbineproxy-token'
const getToken = () => localStorage.getItem(TOKEN_KEY) || ''
const setToken = (t) => t ? localStorage.setItem(TOKEN_KEY, t) : localStorage.removeItem(TOKEN_KEY)

// ─── Route helpers (mirrors App.jsx logic) ────────────────────────────────────
const ROUTES = {
  overview: 'Overview',
  queries: 'Queries',
  'slow-queries': 'Slow Queries',
  n1: 'N+1 Detector',
  heatmap: 'Heatmap',
  timeseries: 'Time-Series',
  pool: 'Connection Pool',
  backends: 'Backends',
  cluster: 'Cluster',
  users: 'Users',
  'query-rules': 'Query Rules',
  'rewrite-rules': 'Rewrite Rules',
  traces: 'Traces',
  analytics: 'Analytics',
  regressions: 'Regressions',
  errors: 'Errors',
  postgresql: 'PostgreSQL',
  config: 'Config',
}
const hashToTab = (h) => ROUTES[h.replace(/^#\/?/, '')] || 'Overview'

// ─── formatBytes helper (mirrors App.jsx) ────────────────────────────────────
function formatBytes(b) {
  if (b === null || b === undefined) return '–'
  if (b < 1024) return `${b} B`
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`
  if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(1)} MB`
  return `${(b / (1024 * 1024 * 1024)).toFixed(1)} GB`
}

// ─── formatDuration helper (mirrors App.jsx) ─────────────────────────────────
function formatDuration(ms) {
  if (ms === null || ms === undefined) return '–'
  if (ms < 1000) return `${ms.toFixed(1)} ms`
  return `${(ms / 1000).toFixed(2)} s`
}

// ─── Tests ────────────────────────────────────────────────────────────────────
describe('Auth token helpers', () => {
  beforeEach(() => localStorage.clear())

  it('returns empty string when no token is set', () => {
    expect(getToken()).toBe('')
  })

  it('stores and retrieves a token', () => {
    setToken('abc123')
    expect(getToken()).toBe('abc123')
  })

  it('removes the token when setToken is called with empty string', () => {
    setToken('abc123')
    setToken('')
    expect(getToken()).toBe('')
    expect(localStorage.getItem(TOKEN_KEY)).toBeNull()
  })

  it('removes the token when setToken is called with null', () => {
    setToken('abc123')
    setToken(null)
    expect(getToken()).toBe('')
  })
})

describe('Hash → tab routing', () => {
  it('maps #overview to Overview', () => {
    expect(hashToTab('#overview')).toBe('Overview')
  })

  it('maps #/overview to Overview (with leading slash)', () => {
    expect(hashToTab('#/overview')).toBe('Overview')
  })

  it('maps #slow-queries to Slow Queries', () => {
    expect(hashToTab('#slow-queries')).toBe('Slow Queries')
  })

  it('maps #n1 to N+1 Detector', () => {
    expect(hashToTab('#n1')).toBe('N+1 Detector')
  })

  it('maps unknown hash to Overview (default)', () => {
    expect(hashToTab('#unknown-route')).toBe('Overview')
  })

  it('maps empty hash to Overview', () => {
    expect(hashToTab('')).toBe('Overview')
  })

  it('maps all defined routes', () => {
    for (const [hash, label] of Object.entries(ROUTES)) {
      expect(hashToTab(`#${hash}`)).toBe(label)
    }
  })
})

describe('formatBytes', () => {
  it('formats null as dash', () => expect(formatBytes(null)).toBe('–'))
  it('formats undefined as dash', () => expect(formatBytes(undefined)).toBe('–'))
  it('formats 0 as bytes', () => expect(formatBytes(0)).toBe('0 B'))
  it('formats bytes under 1 KB', () => expect(formatBytes(512)).toBe('512 B'))
  it('formats KB range', () => expect(formatBytes(2048)).toBe('2.0 KB'))
  it('formats MB range', () => expect(formatBytes(1.5 * 1024 * 1024)).toBe('1.5 MB'))
  it('formats GB range', () => expect(formatBytes(2 * 1024 * 1024 * 1024)).toBe('2.0 GB'))
})

describe('formatDuration', () => {
  it('formats null as dash', () => expect(formatDuration(null)).toBe('–'))
  it('formats undefined as dash', () => expect(formatDuration(undefined)).toBe('–'))
  it('formats sub-second duration', () => expect(formatDuration(42.5)).toBe('42.5 ms'))
  it('formats exactly 1000 ms as seconds', () => expect(formatDuration(1000)).toBe('1.00 s'))
  it('formats multi-second duration', () => expect(formatDuration(2500)).toBe('2.50 s'))
})
