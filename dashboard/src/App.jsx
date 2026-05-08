import { useState, useEffect, useCallback, useMemo } from 'react'
import { useLingui } from '@lingui/react'
import { activateLocale, LOCALE_LABELS, SUPPORTED } from './i18n.js'
import { ConfigPanel } from './ConfigPanel.jsx'
import { ErrorsPanel } from './ErrorsPanel.jsx'
import {
  LayoutDashboard, Database, Clock, Zap, LayoutGrid, TrendingUp,
  Layers, Server, Network, Users as UsersIcon, Filter, Pencil,
  Activity, BarChart2, AlertTriangle, Monitor, Sun, Moon,
  Flame, Search, ChevronRight, ChevronDown,
  CheckCircle, ExternalLink, Download, ArrowUp, ArrowDown, ArrowUpDown,
  LogOut, Lock, RotateCw, Settings, AlertCircle,
} from 'lucide-react'
import turbineLogo from './assets/icon.svg'
import './App.css'

// ─── Auth helpers ─────────────────────────────────────────────────────────────
const TOKEN_KEY = 'turbineproxy-token'
function getToken() { return localStorage.getItem(TOKEN_KEY) || '' }
function setToken(t) { t ? localStorage.setItem(TOKEN_KEY, t) : localStorage.removeItem(TOKEN_KEY) }

function authHeaders() {
  const t = getToken()
  return t ? { 'X-Auth-Token': t } : {}
}

// ─── URL routing ─────────────────────────────────────────────────────────────
const ROUTES = {
  overview:    'Overview',
  queries:     'Queries',
  'slow-queries': 'Slow Queries',
  'n1':        'N+1 Detector',
  heatmap:     'Heatmap',
  timeseries:  'Time-Series',
  pool:        'Connection Pool',
  backends:    'Backends',
  cluster:     'Cluster',
  users:       'Users',
  'query-rules':   'Query Rules',
  'rewrite-rules': 'Rewrite Rules',
  traces:      'Traces',
  analytics:   'Analytics',
  regressions: 'Regressions',
  errors:      'Errors',
  postgresql:  'PostgreSQL',
  config:      'Config',
}
// inverse: tab id → hash path
const TAB_TO_HASH = Object.fromEntries(Object.entries(ROUTES).map(([h, t]) => [t, h]))

function useHashTab() {
  const hashToTab = (h) => ROUTES[h.replace(/^#\/?/, '')] || 'Overview'
  const [tab, setTabState] = useState(() => hashToTab(window.location.hash))

  useEffect(() => {
    const handler = () => setTabState(hashToTab(window.location.hash))
    window.addEventListener('hashchange', handler)
    return () => window.removeEventListener('hashchange', handler)
  }, [])

  const setTab = useCallback((id) => {
    const hash = TAB_TO_HASH[id] || 'overview'
    window.history.pushState(null, '', `#/${hash}`)
    setTabState(id)
  }, [])

  return [tab, setTab]
}

function useFetch(url, intervalMs = 5000) {
  const [data, setData] = useState(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)

  const load = useCallback(async () => {
    try {
      const res = await fetch(url, { headers: authHeaders() })
      if (res.status === 401) {
        setToken('')
        window.location.reload()
        return
      }
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      setData(await res.json())
      setError(null)
    } catch (e) {
      setError(e.message)
    } finally {
      setLoading(false)
    }
  }, [url])

  useEffect(() => {
    load()
    const id = setInterval(load, intervalMs)
    return () => clearInterval(id)
  }, [load, intervalMs])

  return { data, loading, error }
}

function fmtUs(us) {
  if (us == null) return '—'
  if (us < 1000) return `${us}µs`
  if (us < 1_000_000) return `${(us / 1000).toFixed(1)}ms`
  return `${(us / 1_000_000).toFixed(2)}s`
}

function fmtAgo(unixSecs) {
  if (!unixSecs) return ''
  const diff = Math.floor(Date.now() / 1000) - unixSecs
  if (diff < 60) return `${diff}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
}

function latencyBadge(p95_us) {
  if (p95_us == null) return null
  const ms = p95_us / 1000
  if (ms < 10) return <span className="badge fast">&lt;10ms</span>
  if (ms < 100) return <span className="badge slow">&lt;100ms</span>
  return <span className="badge very-slow">&gt;100ms</span>
}

function Spinner() {
  return <div className="center"><div className="spinner" />Loading…</div>
}

function Empty({ msg = 'No data yet.' }) {
  const { _ } = useLingui()
  return (
    <div style={{
      background: 'var(--surface)',
      border: '1px solid var(--border)',
      borderRadius: 8,
      padding: '40px 24px',
      textAlign: 'center',
      color: 'var(--muted)',
      fontSize: '0.9rem',
    }}>
      {_(msg)}
    </div>
  )
}

// ─── Table utilities: search, sort, paginate ──────────────────────────────

const PAGE_SIZE_OPTIONS = [25, 50, 100]

/** Hook that manages search text, sort column/direction, and pagination. */
function useTableControls(rows, { searchKeys = [], defaultSort = null, defaultDir = 'asc' } = {}) {
  const [search, setSearch]     = useState('')
  const [sortKey, setSortKey]   = useState(defaultSort)
  const [sortDir, setSortDir]   = useState(defaultDir)
  const [page, setPage]         = useState(1)
  const [pageSize, setPageSize] = useState(25)

  const filtered = useMemo(() => {
    if (!rows) return []
    const q = search.trim().toLowerCase()
    if (!q || searchKeys.length === 0) return rows
    return rows.filter(r =>
      searchKeys.some(k => String(r[k] ?? '').toLowerCase().includes(q))
    )
  }, [rows, search, searchKeys])

  const sorted = useMemo(() => {
    if (!sortKey) return filtered
    return [...filtered].sort((a, b) => {
      const av = a[sortKey] ?? ''
      const bv = b[sortKey] ?? ''
      const cmp = typeof av === 'number' && typeof bv === 'number'
        ? av - bv
        : String(av).localeCompare(String(bv))
      return sortDir === 'asc' ? cmp : -cmp
    })
  }, [filtered, sortKey, sortDir])

  const totalPages = Math.max(1, Math.ceil(sorted.length / pageSize))
  const safePage   = Math.min(page, totalPages)
  const paginated  = sorted.slice((safePage - 1) * pageSize, safePage * pageSize)

  function toggleSort(key) {
    if (sortKey === key) {
      setSortDir(d => d === 'asc' ? 'desc' : 'asc')
    } else {
      setSortKey(key)
      setSortDir('asc')
    }
    setPage(1)
  }

  function handleSearch(v) {
    setSearch(v)
    setPage(1)
  }

  function handlePageSize(n) {
    setPageSize(n)
    setPage(1)
  }

  return {
    search, onSearch: handleSearch,
    sortKey, sortDir, toggleSort,
    page: safePage, setPage, pageSize, onPageSize: handlePageSize,
    totalPages, total: filtered.length,
    rows: paginated,
  }
}

/** Clickable sort-aware table header cell */
function SortTh({ label, colKey, sortKey, sortDir, onSort, style }) {
  const active = sortKey === colKey
  return (
    <th
      className={`sortable${active ? ' active' : ''}`}
      style={style}
      onClick={() => onSort(colKey)}
    >
      {label}
      <span className="sort-icon">
        {active
          ? (sortDir === 'asc' ? <ArrowUp size={11} /> : <ArrowDown size={11} />)
          : <ArrowUpDown size={11} />}
      </span>
    </th>
  )
}

/** Search box + page size selector + pagination controls */
function TableToolbar({ search, onSearch, page, setPage, pageSize, onPageSize, totalPages, total, placeholder = 'Search…' }) {
  const { _ } = useLingui()
  return (
    <div className="tbl-toolbar">
      {/* Search */}
      <div className="tbl-search-wrap">
        <span className="tbl-search-icon"><Search size={13} /></span>
        <input
          type="text"
          value={search}
          onChange={e => onSearch(e.target.value)}
          placeholder={_(placeholder)}
          className="tbl-search"
        />
      </div>

      <div className="tbl-spacer" />

      <span className="tbl-count">{total.toLocaleString()} {total !== 1 ? _('rows') : _('row')}</span>

      <select
        value={pageSize}
        onChange={e => onPageSize(Number(e.target.value))}
        className="tbl-page-size"
      >
        {PAGE_SIZE_OPTIONS.map(n => <option key={n} value={n}>{n} {_('/ page')}</option>)}
      </select>

      <div className="tbl-pager">
        <button
          disabled={page <= 1}
          onClick={() => setPage(p => p - 1)}
          className="tbl-pager-btn"
          aria-label="Previous page"
        >‹</button>
        <div className="tbl-pager-divider" />
        <span className="tbl-pager-label">{page} / {totalPages}</span>
        <div className="tbl-pager-divider" />
        <button
          disabled={page >= totalPages}
          onClick={() => setPage(p => p + 1)}
          className="tbl-pager-btn"
          aria-label="Next page"
        >›</button>
      </div>
    </div>
  )
}

function StatCard({ label, value, sub }) {
  return (
    <div className="stat-card">
      <div className="label">{label}</div>
      <div className="value">{value ?? '—'}</div>
      {sub && <div className="sub">{sub}</div>}
    </div>
  )
}

// ─── MiniSparkline ───────────────────────────────────────────────────────────
function MiniSparkline({ points, valueKey = 'queries', color = 'var(--accent)' }) {
  if (!points || points.length < 2) {
    return (
      <div style={{ height: 60, display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--muted)', fontSize: 12 }}>
        collecting data…
      </div>
    )
  }
  const W = 500, H = 60, P = 4
  const cw = W - P * 2, ch = H - P * 2
  const vals = points.map(p => p[valueKey] ?? 0)
  const maxV = Math.max(...vals, 1)
  const xs = points.map((_, i) => P + (i / (points.length - 1)) * cw)
  const ys = points.map((p) => P + ch - ((p[valueKey] ?? 0) / maxV) * ch)
  const pathD = xs.map((x, i) => `${i === 0 ? 'M' : 'L'}${x.toFixed(1)},${ys[i].toFixed(1)}`).join(' ')
  const fillD = `${pathD} L${xs[xs.length-1].toFixed(1)},${H} L${P},${H} Z`
  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: '100%', display: 'block', height: 60 }} preserveAspectRatio="none">
      <defs>
        <linearGradient id="miniGrad" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity="0.25" />
          <stop offset="100%" stopColor={color} stopOpacity="0" />
        </linearGradient>
      </defs>
      <path d={fillD} fill="url(#miniGrad)" />
      <path d={pathD} fill="none" stroke={color} strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  )
}

// ─── Overview ────────────────────────────────────────────────────────────────
function Overview() {
  const { _ } = useLingui()
  const { data: statsResp, loading: statsLoading } = useFetch('/api/stats?protocol=auto', 3000)
  const { data: poolResp }    = useFetch('/api/pool?protocol=auto', 5000)
  const { data: regressions } = useFetch('/api/regressions', 15000)
  const { data: slowQResp }   = useFetch('/api/slow-queries?protocol=auto', 15000)
  const { data: ts }          = useFetch('/api/timeseries?resolution=1m&limit=60', 30000)

  const stats = statsResp?.data ?? statsResp
  const pool = poolResp?.pool
  const slowQ = slowQResp?.data ?? slowQResp ?? []

  if (statsLoading && !stats) return <Spinner />

  const readPct = stats?.queries_total > 0
    ? Math.round((stats.queries_read / stats.queries_total) * 100) : 0

  const primaryTotal  = (pool?.primary_idle ?? 0) + (pool?.primary_in_use ?? 0)
  const replicaTotal  = (pool?.replica_idle ?? 0) + (pool?.replica_in_use ?? 0)
  const grandTotal    = primaryTotal + replicaTotal
  const grandInUse    = (pool?.primary_in_use ?? 0) + (pool?.replica_in_use ?? 0)
  const totalCreated  = (pool?.primary_created ?? 0) + (pool?.replica_created ?? 0)
  const totalReused   = (pool?.primary_reused ?? 0) + (pool?.replica_reused ?? 0)
  const totalRequests = totalCreated + totalReused
  const reusePct      = totalRequests > 0 ? (totalReused / totalRequests * 100).toFixed(0) : '—'
  const utilPct       = grandTotal > 0 ? Math.round((grandInUse / grandTotal) * 100) : 0

  const activeAlerts = (regressions ?? []).filter(a => !a.resolved)
  const top5slow     = (slowQ ?? []).slice(0, 5)
  const tsPoints     = ts?.points ?? []
  const tsTotal      = tsPoints.reduce((s, p) => s + (p.queries ?? 0), 0)
  const tsSlow       = tsPoints.reduce((s, p) => s + (p.slow_queries ?? 0), 0)

  return (
    <div>
      {/* ── 8 stat cards ── */}
      <div className="stat-grid">
        <StatCard
          label={_('Active connections')}
          value={stats?.connections_active}
          sub={`${stats?.connections_total ?? 0} ${_('total ever')}`}
        />
        <StatCard
          label={_('Queries total')}
          value={stats?.queries_total?.toLocaleString()}
          sub={`${stats?.queries_read?.toLocaleString() ?? 0} reads · ${stats?.queries_write?.toLocaleString() ?? 0} writes`}
        />
        <StatCard
          label={_('Read ratio')}
          value={`${readPct}%`}
          sub={readPct >= 80 ? _('read-heavy') : readPct >= 50 ? _('balanced') : _('write-heavy')}
        />
        <StatCard
          label={_('Pool reuse')}
          value={pool ? `${reusePct}%` : '—'}
          sub={pool ? `${totalReused.toLocaleString()} ${_('reused')} · ${totalCreated.toLocaleString()} ${_('new TCP')}` : ''}
        />
        <StatCard
          label={_('Pool utilisation')}
          value={pool ? `${utilPct}%` : '—'}
          sub={pool ? `${grandInUse} / ${grandTotal} ${_('connections')}` : ''}
        />
        <StatCard
          label={_('Backend conns')}
          value={pool ? grandTotal : '—'}
          sub={pool
            ? (pool.replica_count > 0
                ? `${primaryTotal} ${_('primary')} · ${pool.replica_count} ${_('replicas')}`
                : _('primary only'))
            : ''}
        />
        <StatCard
          label={_('Active alerts')}
          value={regressions ? activeAlerts.length : '—'}
          sub={activeAlerts.length > 0 ? _('needs attention') : _('all clear')}
        />
        <StatCard
          label={_('Failover')}
          value={pool ? (pool.failover_active ? _('Active') : _('Off')) : '—'}
          sub={pool ? (pool.failover_active ? _('routing to replica') : _('primary healthy')) : ''}
        />
        <StatCard
          label={_('Tx killed')}
          value={stats?.transactions_killed?.toLocaleString() ?? 0}
          sub={_('exceeded max_transaction_time_ms')}
        />
        <StatCard
          label={_('Queries killed')}
          value={stats?.queries_killed?.toLocaleString() ?? 0}
          sub={_('exceeded max_query_time_ms')}
        />
        <StatCard
          label={_('SQLi blocked')}
          value={stats?.sqli_blocked?.toLocaleString() ?? 0}
          sub={_('injection attempts stopped')}
        />
        <StatCard
          label={_('Whitelist blocked')}
          value={stats?.whitelist_blocked?.toLocaleString() ?? 0}
          sub={_('queries not in allowlist')}
        />
      </div>

      {/* ── Sparkline: last 60 min ── */}
      <div className="card" style={{ marginBottom: 20 }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginBottom: 6 }}>
          <span className="section-title" style={{ margin: 0 }}>{_('Query volume — last 60 min')}</span>
          <div style={{ display: 'flex', gap: 14, fontSize: 12, color: 'var(--muted)' }}>
            <span>{tsTotal.toLocaleString()} {_('queries')}</span>
            {tsSlow > 0 && <span style={{ color: 'var(--yellow)' }}>⚑ {tsSlow.toLocaleString()} {_('slow')}</span>}
            <span>{tsPoints.length} {_('points')}</span>
          </div>
        </div>
        <MiniSparkline points={tsPoints} valueKey="queries" color="var(--accent)" />
      </div>

      {/* ── Two-column: slow queries + alerts ── */}
      <div className="overview-two-col" style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 16, marginBottom: 20 }}>

        {/* Top 5 slow queries */}
        <div className="card" style={{ minWidth: 0 }}>
          <div className="section-title">{_('Top 5 Slow Queries (p95)')}</div>
          {top5slow.length === 0 ? (
            <div className="center" style={{ minHeight: 80, fontSize: 12 }}>{_('No slow queries yet.')}</div>
          ) : (
            <div className="table-wrap" style={{ boxShadow: 'none', border: 'none', borderRadius: 0 }}>
              <table>
                <thead>
                  <tr>
                    <th style={{ width: 24 }}>#</th>
                    <th>{_('Fingerprint')}</th>
                    <th>{_('Count')}</th>
                    <th>p95</th>
                    <th>{_('Avg')}</th>
                  </tr>
                </thead>
                <tbody>
                  {top5slow.map((row, i) => {
                    const ms = (row.p95_us ?? 0) / 1000
                    const cls = ms >= 100 ? 'very-slow' : ms >= 10 ? 'slow' : 'fast'
                    return (
                      <tr key={i}>
                        <td style={{ color: 'var(--muted)', fontSize: 11 }}>{i + 1}</td>
                        <td><code className="fp" title={row.fingerprint}>{row.fingerprint}</code></td>
                        <td style={{ whiteSpace: 'nowrap' }}>{row.count?.toLocaleString()}</td>
                        <td style={{ whiteSpace: 'nowrap' }}><span className={`badge ${cls}`}>{fmtUs(row.p95_us)}</span></td>
                        <td style={{ whiteSpace: 'nowrap', color: 'var(--muted)' }}>{fmtUs(row.avg_us)}</td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
        </div>

        {/* Active alerts */}
        <div className="card" style={{ minWidth: 0 }}>
          <div className="section-title">{_('Active Alerts')}</div>
          {activeAlerts.length === 0 ? (
            <div className="center" style={{ minHeight: 80, color: 'var(--green)', gap: 6, fontSize: 13 }}>
              <CheckCircle size={18} />
              {_('No active alerts')}
            </div>
          ) : (
            <div>
              {activeAlerts.slice(0, 5).map((a, i) => (
                <div key={a.id} style={{
                  display: 'flex', alignItems: 'center', gap: 8,
                  padding: '6px 0',
                  borderBottom: i < Math.min(activeAlerts.length, 5) - 1 ? '1px solid var(--border)' : 'none',
                }}>
                  <AlertBadge type={a.details.type} resolved={false} />
                  <code style={{
                    flex: 1, fontSize: 11, overflow: 'hidden', textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap', color: 'var(--muted)',
                  }}>{a.fingerprint}</code>
                </div>
              ))}
              {activeAlerts.length > 5 && (
                <div style={{ fontSize: 11, color: 'var(--muted)', paddingTop: 6 }}>
                  +{activeAlerts.length - 5} more…
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      {/* ── Links row ── */}
      <div className="card" style={{ display: 'flex', alignItems: 'center', gap: 20, flexWrap: 'wrap', padding: '12px 16px' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 13 }}>
          <ExternalLink size={13} style={{ color: 'var(--muted)', flexShrink: 0 }} />
          <span style={{ fontWeight: 600 }}>Prometheus</span>
          <span style={{ color: 'var(--muted)', fontSize: 12 }}>at</span>
          <a href="/metrics" target="_blank" rel="noreferrer" style={{ fontSize: 12, fontFamily: 'monospace' }}>/metrics</a>
        </div>
        <div style={{ width: 1, height: 16, background: 'var(--border)', flexShrink: 0 }} />
        <div style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 13 }}>
          <Download size={13} style={{ color: 'var(--muted)', flexShrink: 0 }} />
          <span style={{ fontWeight: 600 }}>Grafana</span>
          <a href="/grafana/turbineproxy.json" download="turbineproxy-grafana.json" style={{ fontSize: 12 }}>
            {_('download dashboard JSON')}
          </a>
        </div>
      </div>
    </div>
  )
}

function QueriesTable({ url, emptyMsg }) {
  const { _ } = useLingui()
  const { data: payload, loading } = useFetch(url, 10000)
  const data = payload?.data ?? payload ?? []
  const tc = useTableControls(data, {
    searchKeys: ['fingerprint'],
    defaultSort: 'count', defaultDir: 'desc',
  })

  if (loading && !data) return <Spinner />
  if (!data || data.length === 0) return <Empty msg={emptyMsg} />

  const sp = { sortKey: tc.sortKey, sortDir: tc.sortDir, onSort: tc.toggleSort }

  return (
    <div>
      <div className="table-card">
      <TableToolbar {...tc} placeholder="Search fingerprint…" />
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{_('Fingerprint')}</th>
              <SortTh label={_('Count')}     colKey="count"    {...sp} />
              <SortTh label={_('Avg')}       colKey="avg_us"   {...sp} />
              <SortTh label={_('Min')}       colKey="min_us"   {...sp} />
              <SortTh label={_('Max')}       colKey="max_us"   {...sp} />
              <SortTh label="p95"       colKey="p95_us"   {...sp} />
              <SortTh label="p99"       colKey="p99_us"   {...sp} />
              <SortTh label={_('Last seen')} colKey="last_seen" {...sp} />
            </tr>
          </thead>
          <tbody>
            {tc.rows.map((row, i) => (
              <tr key={i}>
                <td><code className="fp" title={row.fingerprint}>{row.fingerprint}</code></td>
                <td>{row.count.toLocaleString()}</td>
                <td>{fmtUs(row.avg_us)}</td>
                <td>{fmtUs(row.min_us)}</td>
                <td>{fmtUs(row.max_us)}</td>
                <td>{latencyBadge(row.p95_us)} {fmtUs(row.p95_us)}</td>
                <td>{fmtUs(row.p99_us)}</td>
                <td style={{ color: 'var(--muted)' }}>
                  {row.last_seen ? new Date(row.last_seen).toLocaleString() : '—'}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {tc.rows.length === 0 && <Empty msg="No results match your search." />}
      </div>
    </div>
  )
}

function N1Table() {
  const { _ } = useLingui()
  const { data, loading } = useFetch('/api/n1', 8000)
  const tc = useTableControls(data, {
    searchKeys: ['fingerprint'],
    defaultSort: 'connections', defaultDir: 'desc',
  })

  if (loading && !data) return <Spinner />
  if (!data || data.length === 0) return <Empty msg="No repeated query patterns detected yet." />

  const sp = { sortKey: tc.sortKey, sortDir: tc.sortDir, onSort: tc.toggleSort }

  return (
    <div>
      <div className="table-card">
      <TableToolbar {...tc} placeholder="Search fingerprint…" />
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{_('Fingerprint')}</th>
              <SortTh label={_('Connections')}  colKey="connections"   {...sp} />
              <SortTh label={_('Max per conn')} colKey="max_per_conn"  {...sp} />
              <SortTh label={_('Last seen')}    colKey="last_seen"     {...sp} />
            </tr>
          </thead>
          <tbody>
            {tc.rows.map((row, i) => (
              <tr key={i}>
                <td><code className="fp" title={row.fingerprint}>{row.fingerprint}</code></td>
                <td>
                  <span className={`badge ${row.connections >= 10 ? 'very-slow' : row.connections >= 3 ? 'slow' : 'fast'}`}>
                    {row.connections}
                  </span>
                </td>
                <td>
                  <span className={`badge ${row.max_per_conn >= 50 ? 'very-slow' : row.max_per_conn >= 20 ? 'slow' : 'fast'}`}>
                    ×{row.max_per_conn}
                  </span>
                </td>
                <td style={{ color: 'var(--muted)' }}>
                  {row.last_seen ? new Date(row.last_seen).toLocaleString() : '—'}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {tc.rows.length === 0 && <Empty msg="No results match your search." />}
      </div>
    </div>
  )
}

function PoolBar({ idle, inUse, label }) {
  const { _ } = useLingui()
  const total = (idle ?? 0) + (inUse ?? 0)
  const usedPct = total > 0 ? Math.round((inUse / total) * 100) : 0
  const barClass = usedPct >= 90 ? 'very-slow' : usedPct >= 60 ? 'slow' : 'fast'
  return (
    <div className="pool-row">
      <div className="pool-row-label">{label}</div>
      <div className="pool-bar-wrap">
        <div className="pool-bar-track">
          <div className={`pool-bar-fill ${barClass}`} style={{ width: `${usedPct}%` }} />
        </div>
        <span className="pool-bar-stat">{inUse ?? 0} {_('in-use')} / {idle ?? 0} {_('idle')}</span>
      </div>
    </div>
  )
}

function PoolPanel({ capabilities }) {
  const { _ } = useLingui()
  const { data: mysql, loading: mysqlLoading } = useFetch('/api/pool?protocol=mysql', 3000)
  const { data: pg } = useFetch('/api/pool?protocol=pgsql', 3000)

  if (mysqlLoading && !mysql && !pg) return <Spinner />

  const mysqlPool = mysql?.pool
  const mysqlPrimaryTotal = (mysqlPool?.primary_idle ?? 0) + (mysqlPool?.primary_in_use ?? 0)
  const mysqlTotalCreated = (mysqlPool?.primary_created ?? 0) + (mysqlPool?.replica_created ?? 0)
  const mysqlTotalReused  = (mysqlPool?.primary_reused ?? 0) + (mysqlPool?.replica_reused ?? 0)
  const mysqlTotalEvicted = (mysqlPool?.primary_evicted ?? 0) + (mysqlPool?.replica_evicted ?? 0)
  const mysqlRequests     = mysqlTotalCreated + mysqlTotalReused
  const mysqlReusePct     = mysqlRequests > 0 ? (mysqlTotalReused / mysqlRequests * 100).toFixed(1) : 0

  const pgEnabled      = pg?.enabled && !!pg?.pool
  const pgPool         = pg?.pool
  const pgPrimaryTotal = (pgPool?.primary_idle ?? 0) + (pgPool?.primary_in_use ?? 0)
  const pgReplicaTotal = (pgPool?.replica_idle ?? 0) + (pgPool?.replica_in_use ?? 0)
  const pgTotalCreated = (pgPool?.primary_created ?? 0) + (pgPool?.replica_created ?? 0)
  const pgTotalReused  = (pgPool?.primary_reused ?? 0) + (pgPool?.replica_reused ?? 0)
  const pgTotalEvicted = (pgPool?.primary_evicted ?? 0) + (pgPool?.replica_evicted ?? 0)
  const pgRequests     = pgTotalCreated + pgTotalReused
  const pgReusePct     = pgRequests > 0 ? (pgTotalReused / pgRequests * 100).toFixed(1) : 0

  const showMysql = capabilities?.mysql_proxy_enabled !== false
  const showPg = capabilities?.pgsql_proxy_enabled === true

  return (
    <div>
      {showMysql && (
      <div className="card" style={{ marginBottom: 14 }}>
        <div className="section-title" style={{ marginBottom: 12 }}>MySQL</div>
        <div className="stat-grid" style={{ marginBottom: 20 }}>
          <StatCard
            label={_('Backend connections')}
            value={mysqlPrimaryTotal}
                sub={`${mysqlPool?.primary_in_use ?? 0} ${_('in-use')} · ${mysqlPool?.primary_idle ?? 0} ${_('idle')}`}
          />
          <StatCard
            label={_('Pool reuse rate')}
            value={`${mysqlReusePct}%`}
            sub={`${mysqlTotalReused} ${_('reused')} · ${mysqlTotalCreated} ${_('new TCP')}`}
          />
          <StatCard
            label={_('Stale evictions')}
            value={mysqlTotalEvicted}
            sub={_('idle connections discarded')}
          />
          <StatCard
            label={_('Failover')}
                value={mysqlPool?.failover_active ? _('Active') : _('Off')}
                sub={mysqlPool?.failover_active ? _('routing to replica') : _('primary healthy')}
          />
        </div>
        <div className="pool-bars">
              <PoolBar label={_('Primary')} idle={mysqlPool?.primary_idle} inUse={mysqlPool?.primary_in_use} />
              {(mysqlPool?.replica_count ?? 0) > 0 && (
                <PoolBar label={`${_('Replicas')} (×${mysqlPool.replica_count})`} idle={mysqlPool?.replica_idle} inUse={mysqlPool?.replica_in_use} />
          )}
        </div>
      </div>
      )}

      {showPg && (
      <div className="card" style={{ marginBottom: 14 }}>
        <div className="section-title" style={{ marginBottom: 12 }}>PostgreSQL</div>
        {pgEnabled ? (
          <>
            <div className="stat-grid" style={{ marginBottom: 20 }}>
              <StatCard
                label={_('Backend connections')}
                value={pgPrimaryTotal + pgReplicaTotal}
                sub={`${pgPool?.primary_in_use ?? 0} ${_('in-use')} · ${pgPool?.primary_idle ?? 0} ${_('idle')}`}
              />
              <StatCard
                label={_('Pool reuse rate')}
                value={`${pgReusePct}%`}
                sub={`${pgTotalReused} ${_('reused')} · ${pgTotalCreated} ${_('new TCP')}`}
              />
              <StatCard
                label={_('Stale evictions')}
                value={pgTotalEvicted}
                sub={_('idle connections discarded')}
              />
              <StatCard
                label={_('COPY active')}
                value={pg?.copy_active ?? 0}
                sub={_('streaming operations')}
              />
            </div>
            <div className="pool-bars">
              <PoolBar label={_('Primary')} idle={pgPool?.primary_idle} inUse={pgPool?.primary_in_use} />
              {(pgPool?.replica_count ?? 0) > 0 && (
                <PoolBar label={`${_('Replicas')} (×${pgPool.replica_count})`} idle={pgPool?.replica_idle} inUse={pgPool?.replica_in_use} />
              )}
            </div>
          </>
        ) : (
          <div style={{ color: 'var(--muted)', fontSize: 13 }}>{_('PostgreSQL proxy is disabled in this environment.')}</div>
        )}
      </div>
      )}

      {!showMysql && !showPg && (
        <Empty msg="No database proxy is enabled in the current configuration." />
      )}
    </div>
  )
}

function UsersPanel() {
  const { _ } = useLingui()
  const { data, loading } = useFetch('/api/users', 5000)
  const tc = useTableControls(data, {
    searchKeys: ['username'],
    defaultSort: 'queries_total', defaultDir: 'desc',
  })

  if (loading && !data) return <Spinner />
  const rows = data ?? []
  if (rows.length === 0) return <Empty msg="No users have connected yet." />

  const sp = { sortKey: tc.sortKey, sortDir: tc.sortDir, onSort: tc.toggleSort }

  return (
    <div>
      <div className="table-card">
      <TableToolbar {...tc} placeholder="Search user…" />
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <SortTh label={_('User')}          colKey="username"           {...sp} />
              <SortTh label={_('Active conns')}  colKey="connections_active" {...sp} />
              <SortTh label={_('Total conns')}   colKey="connections_total"  {...sp} />
              <SortTh label={_('Total queries')} colKey="queries_total"      {...sp} />
              <SortTh label={_('Last seen')}     colKey="last_seen"          {...sp} />
              <th>{_('Writes')}</th>
            </tr>
          </thead>
          <tbody>
            {tc.rows.map(u => (
              <tr key={u.username}>
                <td><strong>{u.username}</strong></td>
                <td>{u.connections_active}</td>
                <td>{u.connections_total}</td>
                <td>{u.queries_total}</td>
                <td>{u.last_seen ?? '—'}</td>
                <td>
                  {u.allow_writes
                    ? <span className="badge fast">{_('read-write')}</span>
                    : <span className="badge very-slow">{_('read-only')}</span>}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {tc.rows.length === 0 && <Empty msg="No results match your search." />}
      </div>
    </div>
  )
}

function QueryRulesPanel() {
  const { data, loading, error } = useFetch('/api/query-rules', 5000)

  if (loading && !data) return <Spinner />

  const rows = data ?? []

  return (
    <div>
      {error && (
        <div style={{ color: 'var(--red)', fontSize: 13, marginBottom: 12 }}>API error: {error}</div>
      )}
      {rows.length === 0 ? (
        <Empty msg="No query rules configured. Add [[query_rules]] to turbineproxy.toml and click Reload." />
      ) : (
        <QueryRulesTable rows={rows} />
      )}
    </div>
  )
}

function QueryRulesTable({ rows }) {
  const { _ } = useLingui()
  const tc = useTableControls(rows, {
    searchKeys: ['match_digest', 'match_pattern', 'user', 'schema', 'comment'],
    defaultSort: 'hit_count', defaultDir: 'desc',
  })
  const sp = { sortKey: tc.sortKey, sortDir: tc.sortDir, onSort: tc.toggleSort }
  return (
    <div>
      <div className="table-card">
      <TableToolbar {...tc} placeholder="Search pattern, user, schema…" />
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{_('#')}</th>
              <th>{_('Pattern / Digest')}</th>
              <SortTh label={_('User')}        colKey="user"        {...sp} />
              <SortTh label={_('Schema')}      colKey="schema"      {...sp} />
              <th>{_('Destination')}</th>
              <th>{_('Cache TTL')}</th>
              <th>{_('Rollout')}</th>
              <th>{_('Mirror')}</th>
              <SortTh label={_('Hits')}       colKey="hit_count"   {...sp} />
              <SortTh label={_('Last match')} colKey="last_match"  {...sp} />
              <th>{_('Comment')}</th>
            </tr>
          </thead>
          <tbody>
            {tc.rows.map((r, i) => {
              const destBadge =
                r.destination === 'primary'  ? <span className="badge very-slow">{_('primary')}</span>
                : r.destination === 'replica' ? <span className="badge fast">{_('replica')}</span>
                : <span className="badge slow">{_('heuristic')}</span>
              const pattern = r.match_digest ?? r.match_pattern ?? '—'
              return (
                <tr key={i}>
                  <td style={{ color: 'var(--muted)' }}>{i + 1}</td>
                  <td><code className="fp" title={pattern}>{pattern}</code></td>
                  <td>{r.user || <span style={{ color: 'var(--muted)' }}>{_('any')}</span>}</td>
                  <td>{r.schema || <span style={{ color: 'var(--muted)' }}>{_('any')}</span>}</td>
                  <td>{destBadge}</td>
                  <td>{r.cache_ttl_secs > 0 ? `${r.cache_ttl_secs}s` : <span style={{ color: 'var(--muted)' }}>{_('off')}</span>}</td>
                  <td>
                    {r.rollout_pct != null
                      ? <span className="badge slow">{r.rollout_pct}%</span>
                      : <span style={{ color: 'var(--muted)' }}>—</span>}
                  </td>
                  <td>
                    {r.mirror_to
                      ? <code style={{ fontSize: 11, color: 'var(--accent)' }}>{r.mirror_to}</code>
                      : <span style={{ color: 'var(--muted)' }}>—</span>}
                  </td>
                  <td>
                    <span className={`badge ${r.hit_count > 0 ? 'fast' : ''}`}>
                      {r.hit_count.toLocaleString()}
                    </span>
                  </td>
                  <td style={{ color: 'var(--muted)' }}>
                    {r.last_match ? new Date(r.last_match).toLocaleString() : '—'}
                  </td>
                  <td style={{ color: 'var(--muted)', fontSize: 12 }}>{r.comment || ''}</td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
      {tc.rows.length === 0 && <Empty msg="No results match your search." />}
      </div>
    </div>
  )
}

function ClusterPanel() {
  const { _ } = useLingui()
  const { data, loading, error } = useFetch('/api/cluster?protocol=auto', 5000)
  const { data: capabilities } = useFetch('/api/capabilities', 20000)
  const [actionBusy, setActionBusy] = useState({ mysql: false, pgsql: false })
  const [actionMsg, setActionMsg] = useState({ mysql: '', pgsql: '' })
  if (loading && !data) return <Spinner />
  if (error) return <div className="center" style={{ color: 'var(--red)' }}>Error: {error}</div>

  const mysql = data?.mysql
  const pg = data?.pgsql
  const showMysql = capabilities?.mysql_proxy_enabled !== false
  const showPg = capabilities?.pgsql_proxy_enabled === true
  const hasAnyVisibleSection = (showMysql && !!mysql) || (showPg && !!pg)

  async function runClusterAction(protocol, action, opts = {}) {
    // Destructive actions require explicit user confirmation.
    if (action === 'trigger_failover') {
      const msg = opts.force
        ? `Force failover for ${protocol.toUpperCase()}? The primary appears healthy — traffic will still be redirected to a replica.`
        : `Trigger failover for ${protocol.toUpperCase()}? This will redirect traffic to a replica.`
      if (!window.confirm(msg)) return
    }
    if (action === 'clear_failover') {
      if (!window.confirm(`Clear failover for ${protocol.toUpperCase()} and return to normal primary routing?`)) return
    }

    setActionBusy(prev => ({ ...prev, [protocol]: true }))
    setActionMsg(prev => ({ ...prev, [protocol]: '' }))
    try {
      const res = await fetch('/api/cluster/actions', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', ...authHeaders() },
        body: JSON.stringify({ protocol, action, force: opts.force ?? false }),
      })
      // If the primary is healthy the backend rejects without force; offer retry
      const payload = await res.json()
      if (!res.ok || payload?.ok === false) {
        const msg = payload?.error ?? `HTTP ${res.status}`
        if (action === 'trigger_failover' && msg.includes('primary is currently healthy')) {
          const confirmForce = window.confirm(
            `${msg}\n\nForce the failover anyway?`
          )
          if (confirmForce) {
            setActionBusy(prev => ({ ...prev, [protocol]: false }))
            return runClusterAction(protocol, action, { force: true })
          }
        }
        throw new Error(msg)
      }
      setActionMsg(prev => ({ ...prev, [protocol]: payload?.message || 'Action completed' }))
    } catch (e) {
      setActionMsg(prev => ({ ...prev, [protocol]: `Error: ${e.message}` }))
    } finally {
      setActionBusy(prev => ({ ...prev, [protocol]: false }))
      setTimeout(() => {
        setActionMsg(prev => ({ ...prev, [protocol]: '' }))
      }, 4000)
    }
  }

  const renderSection = (title, badge, section) => {
    if (!section) return null
    const members = section.members ?? []
    const primaryAddr = section.primary_addr
    const isStandalone = section.mode === 'standalone'
    const showClusterActions = !isStandalone

    const protocol = section.protocol
    return (
      <div className="card" style={{ marginBottom: 14 }}>
        <div style={{ marginBottom: '12px', display: 'flex', alignItems: 'center', gap: '10px', flexWrap: 'wrap' }}>
          <span className={`badge ${badge}`}>{title}</span>
          <span className="badge" style={{ textTransform: 'uppercase' }}>{section.mode}</span>
          {section.failover_active && <span className="badge very-slow">{_('failover active')}</span>}
          {section.patroni_check != null && (
            <span style={{ color: 'var(--muted)', fontSize: 12 }}>Patroni: {section.patroni_check ? _('on') : _('off')}</span>
          )}
          {primaryAddr && (
            <span style={{ color: 'var(--muted)', fontSize: 12 }}>
              {_('Current primary:')} <code>{primaryAddr}</code>
            </span>
          )}

          {showClusterActions && (
            <div style={{ marginLeft: 'auto', display: 'flex', gap: 8, flexWrap: 'wrap', justifyContent: 'flex-end' }}>
              <button
                onClick={() => runClusterAction(protocol, 'recheck_health')}
                disabled={actionBusy[protocol]}
                className="icon-btn"
                style={{
                  padding: '6px 12px',
                  borderRadius: 10,
                  fontSize: 12,
                  fontWeight: 600,
                  lineHeight: 1.1,
                  whiteSpace: 'nowrap',
                }}
                title="Ping all backends and refresh health flags"
              >{_('Recheck health')}</button>
              <button
                onClick={() => runClusterAction(protocol, 'trigger_failover')}
                disabled={actionBusy[protocol]}
                className="icon-btn"
                style={{
                  padding: '6px 12px',
                  borderRadius: 10,
                  fontSize: 12,
                  fontWeight: 600,
                  lineHeight: 1.1,
                  whiteSpace: 'nowrap',
                }}
                title="Force failover to the healthiest replica"
              >{_('Trigger failover')}</button>
              <button
                onClick={() => runClusterAction(protocol, 'clear_failover')}
                disabled={actionBusy[protocol]}
                className="icon-btn"
                style={{
                  padding: '6px 12px',
                  borderRadius: 10,
                  fontSize: 12,
                  fontWeight: 600,
                  lineHeight: 1.1,
                  whiteSpace: 'nowrap',
                }}
                title="Clear manual failover and return to normal primary routing"
              >{_('Clear failover')}</button>
            </div>
          )}
        </div>

        {actionMsg[protocol] && (
          <div style={{ marginBottom: 10, fontSize: 12, color: actionMsg[protocol].startsWith('Error:') ? 'var(--red)' : 'var(--muted)' }}>
            {actionMsg[protocol]}
          </div>
        )}

        {isStandalone && members.length === 0 ? (
          <div style={{ color: 'var(--muted)', fontSize: 13 }}>{_('No cluster members reported.')}</div>
        ) : (
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>{_('Address')}</th>
                  <th>{_('Role')}</th>
                  <th>{_('State')}</th>
                  <th>{_('Lag')}</th>
                  <th>{_('Failures')}</th>
                  <th>{_('Version')}</th>
                </tr>
              </thead>
              <tbody>
                {members.map((m, i) => {
                  const role = (m.role || '').toUpperCase()
                  const isPrimary = role === 'PRIMARY'
                  const state = m.state || _('UNKNOWN')
                  const healthy = m.healthy
                  return (
                    <tr key={`${title}-${i}`} style={isPrimary ? { background: 'rgba(var(--accent-rgb,99,102,241),0.06)' } : {}}>
                      <td><code>{m.addr}</code></td>
                      <td>{isPrimary ? <span className="badge very-slow">{_('PRIMARY')}</span> : <span className="badge fast">{_('REPLICA')}</span>}</td>
                      <td>
                        {healthy === false
                          ? <span className="badge very-slow">{state}</span>
                          : state === _('ONLINE') || state === 'ONLINE'
                            ? <span className="badge fast">{state}</span>
                            : <span className="badge slow">{state}</span>}
                      </td>
                      <td>{m.lag_ms != null ? `${m.lag_ms}ms` : <span style={{ color: 'var(--muted)' }}>—</span>}</td>
                      <td>{m.consecutive_failures != null ? m.consecutive_failures : <span style={{ color: 'var(--muted)' }}>—</span>}</td>
                      <td style={{ color: 'var(--muted)', fontSize: '0.85rem' }}>{m.version || '—'}</td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    )
  }

  return (
    <div>
      {showMysql && renderSection(_('MySQL Cluster'), 'fast', mysql)}
      {showPg && renderSection(_('PostgreSQL HA'), 'slow', pg)}
      {!hasAnyVisibleSection ? (
        <div className="center" style={{ padding: '24px 0', color: 'var(--muted)' }}>
          {_('No cluster/HA information available.')}
        </div>
      ) : null}
    </div>
  )
}

function BackendsPanel({ capabilities }) {
  const { _ } = useLingui()
  const { data, loading, error } = useFetch('/api/backends?protocol=mysql', 3000)
  const { data: pg } = useFetch('/api/backends?protocol=pgsql', 3000)
  const tc = useTableControls(data, {
    searchKeys: ['addr', 'role'],
    defaultSort: 'reused', defaultDir: 'desc',
  })

  if (loading && !data) return <Spinner />
  if (error) return <div className="center" style={{ color: 'var(--red)' }}>Error: {error}</div>

  const sp = { sortKey: tc.sortKey, sortDir: tc.sortDir, onSort: tc.toggleSort }

  const showMysql = capabilities?.mysql_proxy_enabled !== false
  const showPg = capabilities?.pgsql_proxy_enabled === true

  return (
    <div>
      {showMysql && (
      <div className="card" style={{ marginBottom: 14 }}>
        <div className="section-title" style={{ marginBottom: 10 }}>MySQL</div>
      <div className="table-card">
      <TableToolbar {...tc} placeholder="Search address, role…" />
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <SortTh label={_('Address')}   colKey="addr"                 {...sp} />
              <SortTh label={_('Role')}      colKey="role"                 {...sp} />
              <th>{_('HG')}</th>
              <SortTh label={_('Weight')}    colKey="weight"               {...sp} />
              <th>{_('Backup')}</th>
              <th>{_('Status')}</th>
              <SortTh label={_('Lag')}       colKey="lag_ms"               {...sp} />
              <SortTh label={_('Failures')}  colKey="consecutive_failures" {...sp} />
              <SortTh label={_('Idle')}      colKey="idle"                 {...sp} />
              <SortTh label={_('In-use')}    colKey="in_use"               {...sp} />
              <SortTh label={_('Created')}   colKey="created"              {...sp} />
              <SortTh label={_('Reused')}    colKey="reused"               {...sp} />
              <SortTh label={_('Evicted')}   colKey="evicted"              {...sp} />
            </tr>
          </thead>
          <tbody>
            {tc.rows.map((b, i) => {
              const statusBadge = b.healthy
                ? <span className="badge fast">{_('healthy')}</span>
                : <span className="badge very-slow">{_('unhealthy')}</span>
              const lagCell = b.role === 'replica'
                ? (b.lag_ms > 5000
                    ? <span className="badge very-slow">{b.lag_ms}ms</span>
                    : b.lag_ms > 0
                      ? <span className="badge slow">{b.lag_ms}ms</span>
                      : <span style={{ color: 'var(--muted)' }}>0ms</span>)
                : <span style={{ color: 'var(--muted)' }}>—</span>
              return (
                <tr key={i}>
                  <td><code>{b.addr}</code></td>
                  <td>
                    {b.role === 'primary'
                      ? <span className="badge very-slow">{_('primary')}</span>
                      : <span className="badge fast">{_('replica')}</span>}
                  </td>
                  <td style={{ color: 'var(--muted)' }}>{b.hostgroup}</td>
                  <td>{b.weight}</td>
                  <td>
                    {b.backup
                      ? <span className="badge slow">{_('backup')}</span>
                      : <span style={{ color: 'var(--muted)' }}>—</span>}
                  </td>
                  <td>{statusBadge}</td>
                  <td>{lagCell}</td>
                  <td>
                    {b.consecutive_failures > 0
                      ? <span className="badge very-slow">{b.consecutive_failures}</span>
                      : <span style={{ color: 'var(--muted)' }}>0</span>}
                  </td>
                  <td>{b.idle}</td>
                  <td>{b.in_use}</td>
                  <td>{b.created.toLocaleString()}</td>
                  <td>{b.reused.toLocaleString()}</td>
                  <td>{b.evicted}</td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
      {tc.rows.length === 0 && <Empty msg="No results match your search." />}
      </div>
      </div>
      )}

      {showPg && (
      <div className="card" style={{ marginBottom: 14 }}>
        <div className="section-title" style={{ marginBottom: 10 }}>PostgreSQL</div>
        {showPg ? (
          <div className="table-card">
            <div className="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th>{_('Address')}</th>
                    <th>{_('Role')}</th>
                    <th>{_('Status')}</th>
                    <th>{_('Lag')}</th>
                    <th>{_('Failures')}</th>
                    <th>{_('Idle')}</th>
                    <th>{_('In-use')}</th>
                    <th>{_('Created')}</th>
                    <th>{_('Reused')}</th>
                    <th>{_('Evicted')}</th>
                  </tr>
                </thead>
                <tbody>
                  {(pg ?? []).map((b, i) => (
                    <tr key={`pg-${i}`}>
                      <td><code>{b.addr}</code></td>
                      <td>{b.role === 'primary' ? <span className="badge very-slow">{_('primary')}</span> : <span className="badge fast">{_('replica')}</span>}</td>
                      <td>{b.healthy ? <span className="badge fast">{_('healthy')}</span> : <span className="badge very-slow">{_('unhealthy')}</span>}</td>
                      <td>{b.role === 'replica' ? `${b.lag_ms ?? 0}ms` : <span style={{ color: 'var(--muted)' }}>—</span>}</td>
                      <td>{b.consecutive_failures ?? 0}</td>
                      <td>{b.idle}</td>
                      <td>{b.in_use}</td>
                      <td>{(b.created ?? 0).toLocaleString()}</td>
                      <td>{(b.reused ?? 0).toLocaleString()}</td>
                      <td>{b.evicted ?? 0}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {(pg ?? []).length === 0 && <Empty msg="No PostgreSQL backends reported." />}
          </div>
        ) : (
          <div style={{ color: 'var(--muted)', fontSize: 13 }}>PostgreSQL proxy is disabled in this environment.</div>
        )}
      </div>
      )}

      {!showMysql && !showPg && (
        <Empty msg="No backend topology available for the current configuration." />
      )}
    </div>
  )
}

// ─── Theme hook ─────────────────────────────────────────────────────────────
function useTheme() {
  const [theme, setTheme] = useState(() => localStorage.getItem('turbineproxy-theme') || 'system')
  useEffect(() => {
    const html = document.documentElement
    if (theme === 'system') {
      html.removeAttribute('data-theme')
    } else {
      html.dataset.theme = theme
    }
    localStorage.setItem('turbineproxy-theme', theme)
  }, [theme])
  return [theme, setTheme]
}

function ThemeToggle() {
  const [theme, setTheme] = useTheme()
  const icons = { system: <Monitor size={16} />, light: <Sun size={16} />, dark: <Moon size={16} /> }
  const next  = { system: 'light', light: 'dark', dark: 'system' }
  return (
    <button
      className="icon-btn"
      onClick={() => setTheme(t => next[t])}
      title={`Theme: ${theme} (click to cycle)`}
    >
      {icons[theme]}
    </button>
  )
}

function LangSelector() {
  const [current, setCurrent] = useState(
    () => localStorage.getItem('turbineproxy-locale') || navigator.language?.slice(0, 2) || 'en'
  )
  async function handleChange(e) {
    const locale = e.target.value
    setCurrent(locale)
    await activateLocale(locale)
  }
  return (
    <select className="lang-select" value={current} onChange={handleChange}>
      {SUPPORTED.map(l => (
        <option key={l} value={l}>{LOCALE_LABELS[l]}</option>
      ))}
    </select>
  )
}

// ─── Nav groups ──────────────────────────────────────────────────────────────
function makeNavGroups(_, capabilities) {
  const showMysql = capabilities?.mysql_proxy_enabled !== false
  const showPg = capabilities?.pgsql_proxy_enabled === true
  const showCluster = (showMysql && capabilities?.group_replication_enabled === true) || showPg

  return [
    { id: 'performance', label: _('Performance'), items: [
      { id: 'Overview',       label: _('Overview'),       Icon: LayoutDashboard },
      { id: 'Queries',        label: _('Queries'),        Icon: Database },
      { id: 'Slow Queries',   label: _('Slow Queries'),   Icon: Clock },
      { id: 'N+1 Detector',   label: _('N+1 Detector'),   Icon: Zap },
      { id: 'Heatmap',        label: _('Heatmap'),        Icon: LayoutGrid },
    ]},
    { id: 'timeseries', label: _('Time Series'), items: [
      { id: 'Time-Series',    label: _('Time-Series'),    Icon: TrendingUp },
    ]},
    { id: 'infra', label: _('Infrastructure'), items: [
      { id: 'Connection Pool',label: _('Connection Pool'),Icon: Layers },
      { id: 'Backends',       label: _('Backends'),       Icon: Server },
      ...(showCluster ? [{ id: 'Cluster', label: _('Cluster'), Icon: Network }] : []),
    ]},
    { id: 'access', label: _('Access Control'), items: [
      { id: 'Users',          label: _('Users'),          Icon: UsersIcon },
      { id: 'Query Rules',    label: _('Query Rules'),    Icon: Filter },
      { id: 'Rewrite Rules',  label: _('Rewrite Rules'),  Icon: Pencil },
    ]},
    { id: 'observability', label: _('Observability'), items: [
      { id: 'Traces',         label: _('Traces'),         Icon: Activity },
      { id: 'Analytics',      label: _('Analytics'),      Icon: BarChart2 },
      { id: 'Regressions',    label: _('Regressions'),    Icon: AlertTriangle },
      { id: 'Errors',         label: _('Errors'),         Icon: AlertCircle },
    ]},
    { id: 'management', label: _('Management'), items: [
      { id: 'Config',         label: _('Config'),         Icon: Settings },
    ]},
  ].filter(group => {
    if (group.id !== 'infra') return true
    if (showMysql || showPg) return true
    return false
  })
}

// ─── RewriteRulesPanel ───────────────────────────────────────────────────────

function RewriteRulesPanel() {
  const { _ } = useLingui()
  const { data, loading, error } = useFetch('/api/rewrite-rules', 5000)
  const tc = useTableControls(data, {
    searchKeys: ['match_pattern', 'replace_with', 'comment'],
    defaultSort: 'hit_count', defaultDir: 'desc',
  })

  if (loading && !data) return <Spinner />

  const rows = data ?? []

  return (
    <div>
      {error && <div style={{ color: 'var(--red)', marginBottom: 12, fontSize: 13 }}>API error: {error}</div>}
      {rows.length === 0 ? (
        <Empty msg="No rewrite rules configured. Add [[rewrite_rules]] to turbineproxy.toml to enable query rewriting." />
      ) : (
        <div className="table-card">
          <TableToolbar {...tc} placeholder="Search pattern…" />
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>{_('#')}</th>
                  <th>{_('Match pattern')}</th>
                  <th>{_('Operations')}</th>
                  <SortTh label={_('Hits')}       colKey="hit_count"        sortKey={tc.sortKey} sortDir={tc.sortDir} onSort={tc.toggleSort} />
                  <SortTh label={_('Last match')} colKey="last_match_secs"  sortKey={tc.sortKey} sortDir={tc.sortDir} onSort={tc.toggleSort} />
                  <th>{_('Comment')}</th>
                </tr>
              </thead>
              <tbody>
                {tc.rows.map((r, i) => {
                  const ops = []
                  if (r.block) ops.push(<span key="block" className="badge very-slow" title="Blocks the query">{_('block')}</span>)
                  if (r.replace_with) ops.push(<span key="replace" className="badge slow" title={`Replace → ${r.replace_with}`}>{_('replace')}</span>)
                  if (r.add_timeout_ms != null) ops.push(<span key="timeout" className="badge" title={`MAX_EXECUTION_TIME(${r.add_timeout_ms})`}>timeout {r.add_timeout_ms}ms</span>)
                  if (r.add_limit != null) ops.push(<span key="limit" className="badge fast" title={`Appends LIMIT ${r.add_limit}`}>LIMIT {r.add_limit}</span>)
                  return (
                    <tr key={i}>
                      <td style={{ color: 'var(--muted)' }}>{i + 1}</td>
                      <td><code className="fp" title={r.match_pattern}>{r.match_pattern}</code></td>
                      <td style={{ display: 'flex', gap: 4, flexWrap: 'wrap' }}>{ops.length ? ops : <span style={{ color: 'var(--muted)' }}>—</span>}</td>
                      <td><span className={`badge ${r.hit_count > 0 ? 'fast' : ''}`}>{r.hit_count.toLocaleString()}</span></td>
                      <td style={{ color: 'var(--muted)' }}>
                        {r.last_match_secs > 0
                          ? new Date(r.last_match_secs * 1000).toLocaleString()
                          : '—'}
                      </td>
                      <td style={{ color: 'var(--muted)', fontSize: 12 }}>{r.comment || ''}</td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
          {tc.rows.length === 0 && <Empty msg="No results match your search." />}
        </div>
      )}
    </div>
  )
}

// ─── TracesPanel ─────────────────────────────────────────────────────────────

const INTENT_COLOR = { read: '#73BF69', write: '#F2495C', transaction: '#FFB357', other: '#8AB8FF' }

function DurationBar({ queries, totalMs }) {
  if (!queries || queries.length === 0) return null
  return (
    <div style={{ display: 'flex', height: 10, borderRadius: 4, overflow: 'hidden', width: '100%', minWidth: 120, maxWidth: 340, background: 'var(--surface3)' }}>
      {queries.map((q, i) => {
        const pct = totalMs > 0 ? Math.max((q.duration_ms / totalMs) * 100, 0.5) : 100 / queries.length
        return (
          <div
            key={i}
            title={`${q.fingerprint}\n${q.duration_ms.toFixed(1)}ms (${q.intent})`}
            style={{ width: `${pct}%`, background: INTENT_COLOR[q.intent] || '#8AB8FF', minWidth: 1 }}
          />
        )
      })}
    </div>
  )
}

function TraceDetail({ trace }) {
  const { _ } = useLingui()
  const total = trace.duration_ms || 1
  return (
    <div style={{ margin: '8px 0 8px 32px', padding: '12px 16px', background: 'var(--surface2)', border: '1px solid var(--border)', borderRadius: 6, fontSize: 12 }}>
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 60px 90px 90px', gap: '4px 8px', fontWeight: 600, color: 'var(--muted)', marginBottom: 6, textTransform: 'uppercase', fontSize: 11 }}>
        <span>{_('Query (fingerprint)')}</span><span>{_('Duration')}</span><span>{_('Intent')}</span><span>{_('Backend')}</span>
      </div>
      {trace.queries.map((q, i) => {
        const barPct = Math.max((q.duration_ms / total) * 100, 0.5)
        return (
          <div key={i} style={{ display: 'grid', gridTemplateColumns: '1fr 60px 90px 90px', gap: '4px 8px', padding: '3px 0', borderTop: '1px solid var(--border)', alignItems: 'center' }}>
            <div>
              <div style={{ height: 4, width: `${barPct}%`, minWidth: 2, background: INTENT_COLOR[q.intent] || '#8AB8FF', borderRadius: 2, marginBottom: 3 }} />
              <code style={{ fontSize: 11, color: 'var(--text2)', wordBreak: 'break-all' }} title={q.sql}>{q.fingerprint}</code>
            </div>
            <span style={{ color: q.duration_ms > 1000 ? 'var(--red)' : q.duration_ms > 100 ? '#FFB357' : 'var(--muted)' }}>
              {q.duration_ms < 1 ? '<1ms' : `${q.duration_ms.toFixed(1)}ms`}
            </span>
            <span style={{ color: INTENT_COLOR[q.intent] || '#8AB8FF', textTransform: 'uppercase', fontSize: 10 }}>{q.intent}</span>
            <span style={{ color: 'var(--muted)', fontFamily: 'monospace', fontSize: 10 }}>{q.backend_addr}</span>
          </div>
        )
      })}
    </div>
  )
}

function TracesPanel() {
  const { _ } = useLingui()
  const [filterFp, setFilterFp] = useState(null)
  const [search, setSearch] = useState('')
  const [page, setPage] = useState(1)
  const [pageSize, setPageSize] = useState(25)

  const url = filterFp
    ? `/api/transactions?limit=200&fingerprint=${encodeURIComponent(filterFp)}`
    : '/api/transactions?limit=200'
  const { data, loading, error } = useFetch(url, 5000)
  const [expanded, setExpanded] = useState({})

  const toggle = (id) => setExpanded(prev => ({ ...prev, [id]: !prev[id] }))

  const traces = data?.traces ?? []
  const fingerprints = data?.fingerprints ?? []

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase()
    if (!q) return traces
    return traces.filter(t =>
      t.user?.toLowerCase().includes(q) ||
      t.outcome?.toLowerCase().includes(q) ||
      t.tx_fingerprint?.toLowerCase().includes(q) ||
      t.client_addr?.toLowerCase().includes(q)
    )
  }, [traces, search])

  const totalPages = Math.max(1, Math.ceil(filtered.length / pageSize))
  const safePage   = Math.min(page, totalPages)
  const paginated  = filtered.slice((safePage - 1) * pageSize, safePage * pageSize)

  function handleSearch(v) { setSearch(v); setPage(1) }
  function handlePageSize(n) { setPageSize(n); setPage(1) }

  if (loading && !data) return <Spinner />

  return (
    <div>
      {error && <div style={{ color: 'var(--red)', marginBottom: 12, fontSize: 13 }}>API error: {error}</div>}

      {/* Fingerprint filter chips */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 12, flexWrap: 'wrap' }}>
        <span style={{ fontSize: 12, color: 'var(--muted)' }}>{_('Filter by pattern:')}</span>
        <button
          onClick={() => { setFilterFp(null); setPage(1) }}
          style={{
            fontSize: 11, padding: '3px 10px', borderRadius: 12, border: '1px solid',
            borderColor: filterFp === null ? 'var(--accent)' : 'var(--border)',
            background: filterFp === null ? 'var(--accent)' : 'transparent',
            color: filterFp === null ? '#fff' : 'var(--muted)', cursor: 'pointer'
          }}
        >{_('All')}</button>
        {fingerprints.slice(0, 8).map(({ fingerprint, count }) => (
          <button
            key={fingerprint}
            onClick={() => { setFilterFp(fingerprint === filterFp ? null : fingerprint); setPage(1) }}
            title={`${count} traces with this pattern`}
            style={{
              fontSize: 11, padding: '3px 10px', borderRadius: 12, border: '1px solid',
              borderColor: filterFp === fingerprint ? 'var(--accent)' : 'var(--border)',
              background: filterFp === fingerprint ? 'var(--accent)' : 'transparent',
              color: filterFp === fingerprint ? '#fff' : 'var(--text)', cursor: 'pointer',
              fontFamily: 'monospace'
            }}
          >{fingerprint.slice(0, 8)}<span style={{ color: 'var(--muted)', marginLeft: 4 }}>×{count}</span></button>
        ))}
      </div>

      {/* Search + pagination toolbar */}
      <div className="table-card">
      <TableToolbar
        search={search} onSearch={handleSearch}
        page={safePage} setPage={setPage}
        pageSize={pageSize} onPageSize={handlePageSize}
        totalPages={totalPages} total={filtered.length}
        placeholder="Search user, outcome, fingerprint…"
      />

      {paginated.length === 0 ? (
        <Empty msg="No transaction traces yet. Transactions (BEGIN … COMMIT/ROLLBACK) are captured automatically." />
      ) : (
        <div>
          {/* Header row */}
          <div style={{ display: 'grid', gridTemplateColumns: '32px 120px 1fr 80px 60px 80px 100px', gap: '4px 8px', padding: '10px 14px', fontSize: 11, fontWeight: 700, textTransform: 'uppercase', letterSpacing: '.06em', color: 'var(--muted)', background: 'var(--surface2)', borderBottom: '1px solid var(--border)' }}>
            <span />
            <span>{_('User')}</span>
            <span>{_('Waterfall')}</span>
            <span>{_('Duration')}</span>
            <span>{_('Queries')}</span>
            <span>{_('Outcome')}</span>
            <span>{_('Pattern')}</span>
          </div>
          {paginated.map(trace => (
            <div key={trace.id}>
              <div
                onClick={() => toggle(trace.id)}
                style={{ display: 'grid', gridTemplateColumns: '32px 120px 1fr 80px 60px 80px 100px', gap: '4px 8px', padding: '8px', cursor: 'pointer', borderBottom: '1px solid var(--border)', alignItems: 'center' }}
                className="table-row-hover"
              >
                <span style={{ color: 'var(--muted)', display: 'flex', alignItems: 'center' }}>
                  {expanded[trace.id] ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                </span>
                <span style={{ fontFamily: 'monospace', fontSize: 11, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={`${trace.user}@${trace.client_addr}`}>{trace.user}</span>
                <DurationBar queries={trace.queries} totalMs={trace.duration_ms} />
                <span style={{ fontSize: 12, color: trace.duration_ms > 1000 ? 'var(--red)' : trace.duration_ms > 200 ? '#FFB357' : 'inherit' }}>
                  {trace.duration_ms < 1 ? '<1ms' : `${trace.duration_ms.toFixed(1)}ms`}
                </span>
                <span style={{ fontSize: 12, color: 'var(--muted)' }}>{trace.query_count}</span>
                <span className={`badge ${trace.outcome === 'commit' ? 'fast' : trace.outcome === 'rollback' ? 'slow' : 'very-slow'}`}>
                  {trace.outcome}
                </span>
                <span
                  style={{ fontFamily: 'monospace', fontSize: 10, color: 'var(--muted)', cursor: 'pointer' }}
                  title="Click to filter similar transactions"
                  onClick={e => { e.stopPropagation(); setFilterFp(trace.tx_fingerprint); setPage(1) }}
                >
                  {trace.tx_fingerprint.slice(0, 8)}
                </span>
              </div>
              {expanded[trace.id] && <TraceDetail trace={trace} />}
            </div>
          ))}
        </div>
      )}
      </div>
    </div>
  )
}

// ─── AnalyticsPanel ──────────────────────────────────────────────────────────

function DimBar({ value, max }) {
  const pct = max > 0 ? Math.max((value / max) * 100, 0.5) : 0
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 120 }}>
      <div style={{ flex: 1, height: 6, background: 'var(--surface3)', borderRadius: 3, overflow: 'hidden' }}>
        <div style={{ width: `${pct}%`, height: '100%', background: 'var(--accent)', borderRadius: 3 }} />
      </div>
      <span style={{ fontSize: 11, color: 'var(--muted)', minWidth: 36, textAlign: 'right' }}>{value.toLocaleString()}</span>
    </div>
  )
}

function DimTable({ rows, label }) {
  const { _ } = useLingui()
  const tc = useTableControls(rows, {
    searchKeys: ['key'],
    defaultSort: 'queries_total', defaultDir: 'desc',
  })

  if (!rows || rows.length === 0) return <Empty msg={`No ${label} data yet.`} />
  const maxQ = rows[0]?.queries_total || 1
  const sp = { sortKey: tc.sortKey, sortDir: tc.sortDir, onSort: tc.toggleSort }

  return (
    <div>
      <div className="table-card">
      <TableToolbar {...tc} placeholder={`${_('Search…')}`} />
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{label}</th>
              <SortTh label={_('Queries')}      colKey="queries_total"      {...sp} />
              <SortTh label={_('Read %')}       colKey="queries_read"       {...sp} />
              <SortTh label={_('Active conns')} colKey="connections_active" {...sp} />
              <SortTh label={_('Total conns')}  colKey="connections_total"  {...sp} />
              <SortTh label={_('Last seen')}    colKey="last_seen_ms"       {...sp} />
            </tr>
          </thead>
          <tbody>
            {tc.rows.map((r, i) => {
              const readPct = r.queries_total > 0 ? Math.round((r.queries_read / r.queries_total) * 100) : 0
              const lastSeen = r.last_seen_ms > 0
                ? new Date(r.last_seen_ms).toLocaleString()
                : '—'
              return (
                <tr key={i}>
                  <td style={{ fontFamily: 'monospace', fontSize: 12 }}>{r.key || '—'}</td>
                  <td style={{ minWidth: 160 }}><DimBar value={r.queries_total} max={maxQ} /></td>
                  <td>
                    <span className={`badge ${readPct >= 80 ? 'fast' : readPct >= 40 ? '' : 'very-slow'}`}>{readPct}%</span>
                  </td>
                  <td style={{ fontSize: 12 }}>{r.connections_active}</td>
                  <td style={{ fontSize: 12, color: 'var(--muted)' }}>{r.connections_total}</td>
                  <td style={{ fontSize: 12, color: 'var(--muted)' }}>{lastSeen}</td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>
      {tc.rows.length === 0 && <Empty msg="No results match your search." />}
      </div>
    </div>
  )
}

function AnalyticsPanel() {
  const { _ } = useLingui()
  const { data, loading, error } = useFetch('/api/analytics', 5000)
  const [view, setView] = useState('users')

  if (loading && !data) return <Spinner />

  const tabs = [
    { key: 'users', label: _('By User') },
    { key: 'ips',   label: _('By IP') },
    { key: 'apps',  label: _('By App') },
  ]

  const rows = data?.[view] ?? []

  return (
    <div>
      {error && <div style={{ color: 'var(--red)', marginBottom: 12, fontSize: 13 }}>API error: {error}</div>}

      {/* Summary stat cards */}
      {data && (
        <div className="stat-grid" style={{ marginBottom: 20 }}>
          <StatCard label={_('Distinct users')} value={data.users?.length ?? 0} sub={_('seen since start')} />
          <StatCard label={_('Distinct IPs')} value={data.ips?.length ?? 0} sub={_('client addresses')} />
          <StatCard label={_('Distinct apps')} value={data.apps?.length ?? 0} sub={_('from _program_name')} />
          <StatCard
            label={_('Most active user')}
            value={data.users?.[0]?.key || '—'}
            sub={data.users?.[0] ? `${data.users[0].queries_total.toLocaleString()} ${_('queries')}` : ''}
          />
        </div>
      )}

      {/* Sub-tab selector */}
      <div style={{ display: 'flex', gap: 4, marginBottom: 14 }}>
        {tabs.map(t => (
          <button
            key={t.key}
            onClick={() => setView(t.key)}
            style={{
              fontSize: 12, padding: '5px 14px', borderRadius: 6, border: '1px solid',
              borderColor: view === t.key ? 'var(--accent)' : 'var(--border)',
              background: view === t.key ? 'var(--accent)' : 'transparent',
              color: view === t.key ? '#fff' : 'var(--text)', cursor: 'pointer', fontWeight: 600,
            }}
          >{t.label} {data ? `(${data[t.key]?.length ?? 0})` : ''}</button>
        ))}
      </div>

      <DimTable rows={rows} label={tabs.find(t => t.key === view)?.label ?? view} />
    </div>
  )
}

// ─── HeatmapPanel ────────────────────────────────────────────────────────────

const HOUR_LABELS = Array.from({ length: 24 }, (_, i) => `${String(i).padStart(2, '0')}h`)

function heatColor(value, max, isAnomaly) {
  if (max === 0 || value === 0) return 'var(--surface2)'
  if (isAnomaly) return '#F2495C'
  const pct = value / max
  if (pct < 0.25) return '#1e3a5f'
  if (pct < 0.5)  return '#1565a8'
  if (pct < 0.75) return '#2196f3'
  return '#64b5f6'
}

function HeatmapPanel() {
  const { _ } = useLingui()
  const DAY_LABELS = [_('Sun'), _('Mon'), _('Tue'), _('Wed'), _('Thu'), _('Fri'), _('Sat')]
  const { data, loading, error } = useFetch('/api/heatmap', 10000)
  const [mode, setMode] = useState('queries') // 'queries' | 'slow'

  if (loading && !data) return <Spinner />

  const cells = data?.cells ?? []
  const values = cells.map(c => mode === 'slow' ? c.slow : c.queries)
  const maxVal = Math.max(...values, 1)

  // Build a map (day * 24 + hour) → cell
  const grid = {}
  for (const c of cells) {
    grid[c.day * 24 + c.hour] = c
  }

  return (
    <div>
      {error && <div style={{ color: 'var(--red)', marginBottom: 12, fontSize: 13 }}>API error: {error}</div>}

      {/* Stats row */}
      {data && (
        <div className="stat-grid" style={{ marginBottom: 20 }}>
          <StatCard label={_('Total queries')} value={data.total_queries?.toLocaleString()} sub={_('all time')} />
          <StatCard label={_('Slow queries')} value={data.total_slow?.toLocaleString()} sub={`>${0}ms ${_('threshold')}`} />
          <StatCard
            label={_('Busiest slot')}
            value={data.peaks?.[0] ? `${DAY_LABELS[data.peaks[0].day]} ${HOUR_LABELS[data.peaks[0].hour]}` : '—'}
            sub={data.peaks?.[0] ? `${data.peaks[0].queries.toLocaleString()} ${_('queries')}` : ''}
          />
          <StatCard
            label={_('Anomalies')}
            value={cells.filter(c => c.is_anomaly).length}
            sub={_('cells above 2σ')}
          />
        </div>
      )}

      {/* Mode toggle + legend */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 12, marginBottom: 16, flexWrap: 'wrap' }}>
        {(['queries', 'slow']).map(m => (
          <button key={m} onClick={() => setMode(m)} style={{
            fontSize: 12, padding: '4px 14px', borderRadius: 6, border: '1px solid',
            borderColor: mode === m ? 'var(--accent)' : 'var(--border)',
            background: mode === m ? 'var(--accent)' : 'transparent',
            color: mode === m ? '#fff' : 'var(--text)', cursor: 'pointer', fontWeight: 600,
          }}>{m === 'queries' ? _('Query volume') : _('Slow queries')}</button>
        ))}
        <div style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 11, color: 'var(--muted)', marginLeft: 8 }}>
          <div style={{ width: 12, height: 12, background: 'var(--surface2)', border: '1px solid var(--border)', borderRadius: 2 }} /> {_('empty')}
          <div style={{ width: 12, height: 12, background: '#1e3a5f', borderRadius: 2 }} /> {_('low')}
          <div style={{ width: 12, height: 12, background: '#2196f3', borderRadius: 2 }} /> {_('high')}
          <div style={{ width: 12, height: 12, background: '#F2495C', borderRadius: 2 }} /> {_('anomaly')}
        </div>
      </div>

      {/* Grid: days as rows, hours as columns */}
      <div style={{ overflowX: 'auto', background: 'var(--surface)', border: '1px solid var(--border)', borderRadius: 8, padding: '12px 12px 4px' }}>
        {/* Hour header */}
        <div style={{ display: 'grid', gridTemplateColumns: '36px repeat(24, 1fr)', gap: 2, marginBottom: 2 }}>
          <div />
          {HOUR_LABELS.map(h => (
            <div key={h} style={{ fontSize: 9, color: 'var(--muted)', textAlign: 'center' }}>{h}</div>
          ))}
        </div>
        {/* Day rows */}
        {DAY_LABELS.map((day, d) => (
          <div key={d} style={{ display: 'grid', gridTemplateColumns: '36px repeat(24, 1fr)', gap: 2, marginBottom: 2 }}>
            <div style={{ fontSize: 11, color: 'var(--muted)', display: 'flex', alignItems: 'center', justifyContent: 'flex-end', paddingRight: 6 }}>{day}</div>
            {Array.from({ length: 24 }, (__unused, h) => {
              const cell = grid[d * 24 + h]
              const val = cell ? (mode === 'slow' ? cell.slow : cell.queries) : 0
              const isAn = cell?.is_anomaly ?? false
              const avg = cell?.avg_ms ?? 0
              return (
                <div
                  key={h}
                  title={`${day} ${HOUR_LABELS[h]}: ${val.toLocaleString()} ${mode}\navg ${avg.toFixed(1)}ms${isAn ? `\n⚠ ${_('Anomaly (>2σ)')}` : ''}`}
                  style={{
                    height: 22,
                    borderRadius: 3,
                    background: heatColor(val, maxVal, isAn),
                    cursor: val > 0 ? 'default' : 'default',
                    outline: isAn ? '1px solid #F2495C' : 'none',
                  }}
                />
              )
            })}
          </div>
        ))}
      </div>

      {/* Peaks table */}
      {data?.peaks?.length > 0 && (
        <div style={{ marginTop: 24 }}>
          <div className="section-title">{_('Peak slots')}</div>
          <div className="table-wrap">
            <table>
              <thead><tr><th>{_('Slot')}</th><th>{_('Queries')}</th><th>{_('Slow')}</th><th>{_('Avg latency')}</th><th>{_('Anomaly')}</th></tr></thead>
              <tbody>
                {data.peaks.map((p, i) => (
                  <tr key={i}>
                    <td style={{ fontFamily: 'monospace' }}>{DAY_LABELS[p.day]} {HOUR_LABELS[p.hour]}</td>
                    <td>{p.queries.toLocaleString()}</td>
                    <td style={{ color: p.slow > 0 ? 'var(--yellow)' : 'inherit' }}>{p.slow.toLocaleString()}</td>
                    <td>{p.avg_ms.toFixed(1)}ms</td>
                    <td>{p.is_anomaly ? <span className="badge very-slow">⚠ {_('spike')}</span> : <span className="badge fast">{_('normal')}</span>}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  )
}

// ─── HistóricoPanel ──────────────────────────────────────────────────────────────────────────────

const RES_OPTIONS = [
  { key: '1m', label: '1 min',   limit: 120, unit: 'm' },
  { key: '1h', label: '1 hour',  limit: 168, unit: 'h' },
  { key: '1d', label: '1 day',   limit: 90,  unit: 'd' },
]

const MODE_OPTIONS = [
  { key: 'queries',      label: 'Query volume' },
  { key: 'slow_queries', label: 'Slow queries' },
  { key: 'avg_us',       label: 'Avg latency (µs)' },
]

function TsLineChart({ points, valueKey, color }) {
  if (!points || points.length < 2) {
    return <div style={{ height: 160, display: 'flex', alignItems: 'center', justifyContent: 'center', color: 'var(--muted)', fontSize: 13 }}>Not enough data yet — chart fills in over time.</div>
  }
  const W = 800
  const H = 140
  const PAD = { top: 10, right: 16, bottom: 32, left: 52 }
  const cw = W - PAD.left - PAD.right
  const ch = H - PAD.top - PAD.bottom

  const values = points.map(p => p[valueKey])
  const maxV = Math.max(...values, 1)

  const xs = points.map((_, i) => PAD.left + (i / (points.length - 1)) * cw)
  const ys = points.map(v => PAD.top + ch - (v[valueKey] / maxV) * ch)

  const pathD = xs.map((x, i) => `${i === 0 ? 'M' : 'L'}${x.toFixed(1)},${ys[i].toFixed(1)}`).join(' ')
  const fillD = pathD + ` L${xs[xs.length - 1].toFixed(1)},${(PAD.top + ch).toFixed(1)} L${xs[0].toFixed(1)},${(PAD.top + ch).toFixed(1)} Z`

  // Y axis ticks (4 levels)
  const yTicks = [0, 0.25, 0.5, 0.75, 1].map(f => ({
    y: PAD.top + ch - f * ch,
    label: formatTsValue(maxV * f, valueKey),
  }))

  // X axis ticks (up to 6 labels)
  const xStep = Math.max(1, Math.floor(points.length / 6))
  const xTicks = points
    .filter((_, i) => i % xStep === 0 || i === points.length - 1)
    .map((p, i) => ({
      x: xs[i * xStep] ?? xs[xs.length - 1],
      label: formatBucket(p.bucket_unix),
    }))

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: '100%', maxWidth: W, display: 'block', overflow: 'visible' }}>
      <defs>
        <linearGradient id="tsGrad" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity="0.25" />
          <stop offset="100%" stopColor={color} stopOpacity="0.02" />
        </linearGradient>
      </defs>
      {/* grid lines */}
      {yTicks.map((t, i) => (
        <g key={i}>
          <line x1={PAD.left} y1={t.y} x2={PAD.left + cw} y2={t.y} style={{ stroke: 'var(--border)' }} strokeWidth="1" />
          <text x={PAD.left - 6} y={t.y + 4} textAnchor="end" fontSize="10" style={{ fill: 'var(--muted)' }}>{t.label}</text>
        </g>
      ))}
      {/* fill area */}
      <path d={fillD} fill="url(#tsGrad)" />
      {/* line */}
      <path d={pathD} fill="none" stroke={color} strokeWidth="2" strokeLinejoin="round" strokeLinecap="round" />
      {/* x-axis labels */}
      {xTicks.map((t, i) => (
        <text key={i} x={t.x} y={H - 4} textAnchor="middle" fontSize="10" style={{ fill: 'var(--muted)' }}>{t.label}</text>
      ))}
    </svg>
  )
}

function formatTsValue(v, key) {
  if (key === 'avg_us') {
    if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}s`
    if (v >= 1_000)     return `${(v / 1_000).toFixed(0)}ms`
    return `${v.toFixed(0)}µs`
  }
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M`
  if (v >= 1_000)     return `${(v / 1_000).toFixed(0)}k`
  return `${Math.round(v)}`
}

function formatBucket(unix) {
  const d = new Date(unix * 1000)
  const hh = String(d.getUTCHours()).padStart(2, '0')
  const mm = String(d.getUTCMinutes()).padStart(2, '0')
  const md = String(d.getUTCDate()).padStart(2, '0')
  return `${md} ${hh}:${mm}`
}

function HistoricoPanel() {
  const { _ } = useLingui()
  const [res, setRes] = useState('1h')
  const [mode, setMode] = useState('queries')
  const opt = RES_OPTIONS.find(r => r.key === res)
  const url = `/api/timeseries?resolution=${res}&limit=${opt.limit}`
  const { data, loading, error } = useFetch(url, 30000)

  if (loading && !data) return <Spinner />

  const points = data?.points ?? []
  const totalQ = points.reduce((s, p) => s + p.queries, 0)
  const totalSlow = points.reduce((s, p) => s + p.slow_queries, 0)
  const maxAvg = points.reduce((s, p) => Math.max(s, p.avg_us), 0)
  const color = mode === 'slow_queries' ? '#F2495C' : mode === 'avg_us' ? '#FFB357' : '#5794F2'

  return (
    <div>
      {error && <div style={{ color: 'var(--red)', marginBottom: 12, fontSize: 13 }}>API error: {error}</div>}

      {/* Stat cards */}
      <div className="stat-grid" style={{ marginBottom: 20 }}>
        <StatCard label={_('Total queries')} value={totalQ.toLocaleString()} sub={`${_('last')} ${opt.limit} ${opt.unit}`} />
        <StatCard label={_('Slow queries')} value={totalSlow.toLocaleString()} sub={_('above threshold')} />
        <StatCard label={_('Peak avg latency')} value={formatTsValue(maxAvg, 'avg_us')} sub={_('per bucket')} />
        <StatCard label={_('Data points')} value={points.length.toLocaleString()} sub={`${_('at')} ${opt.label} ${_('resolution')}`} />
      </div>

      {/* Controls */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 12, marginBottom: 18, flexWrap: 'wrap' }}>
        <div style={{ display: 'flex', gap: 4 }}>
          {RES_OPTIONS.map(r => (
            <button key={r.key} onClick={() => setRes(r.key)} style={{
              fontSize: 12, padding: '4px 12px', borderRadius: 6, border: '1px solid',
              borderColor: res === r.key ? 'var(--accent)' : 'var(--border)',
              background: res === r.key ? 'var(--accent)' : 'transparent',
              color: res === r.key ? '#fff' : 'var(--text)', cursor: 'pointer',
            }}>{r.label}</button>
          ))}
        </div>
        <div style={{ width: 1, height: 22, background: 'var(--border)' }} />
        <div style={{ display: 'flex', gap: 4 }}>
          {MODE_OPTIONS.map(m => (
            <button key={m.key} onClick={() => setMode(m.key)} style={{
              fontSize: 12, padding: '4px 12px', borderRadius: 6, border: '1px solid',
              borderColor: mode === m.key ? color : 'var(--border)',
              background: mode === m.key ? color : 'transparent',
              color: mode === m.key ? '#fff' : 'var(--text)', cursor: 'pointer',
            }}>{m.label}</button>
          ))}
        </div>
      </div>

      {/* Chart */}
      <div style={{ background: 'var(--surface2)', border: '1px solid var(--border)', borderRadius: 10, padding: '16px 12px 8px', marginBottom: 24 }}>
        <TsLineChart points={points} valueKey={mode} color={color} />
      </div>

      {/* Data table */}
      {points.length > 0 && (
        <BucketsTable points={points} />
      )}
    </div>
  )
}

function BucketsTable({ points }) {
  const { _ } = useLingui()
  const reversed = useMemo(() => [...points].reverse(), [points])
  const tc = useTableControls(reversed, {
    defaultSort: 'bucket_unix', defaultDir: 'desc',
  })
  const sp = { sortKey: tc.sortKey, sortDir: tc.sortDir, onSort: tc.toggleSort }
  return (
    <div>
      <div className="section-title">{_('Buckets')}</div>
      <div className="table-card">
      <TableToolbar {...tc} placeholder="" />
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <SortTh label={_('Bucket (UTC)')} colKey="bucket_unix"  {...sp} />
              <SortTh label={_('Queries')}      colKey="queries"       {...sp} />
              <SortTh label={_('Slow')}         colKey="slow_queries"  {...sp} />
              <SortTh label={_('Avg latency')}  colKey="avg_us"        {...sp} />
              <SortTh label={_('Max latency')}  colKey="max_us"        {...sp} />
            </tr>
          </thead>
          <tbody>
            {tc.rows.map((p, i) => (
              <tr key={i}>
                <td style={{ fontFamily: 'monospace', fontSize: 12 }}>{new Date(p.bucket_unix * 1000).toISOString().slice(0, 16).replace('T', ' ')}</td>
                <td>{p.queries.toLocaleString()}</td>
                <td style={{ color: p.slow_queries > 0 ? 'var(--yellow)' : 'inherit' }}>{p.slow_queries.toLocaleString()}</td>
                <td>{formatTsValue(p.avg_us, 'avg_us')}</td>
                <td style={{ color: p.max_us > 1_000_000 ? 'var(--red)' : 'inherit' }}>{formatTsValue(p.max_us, 'avg_us')}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      </div>
    </div>
  )
}

// ─── RegressoesPanel ────────────────────────────────────────────────────────────────────────────

const ALERT_META = {
  latency_regression: { label: 'Latency Regression', color: '#F2495C', Icon: AlertTriangle },
  hot_key:            { label: 'Hot Key',             color: '#FFB357', Icon: Flame },
  full_scan_risk:     { label: 'Full Scan Risk',       color: '#FF9830', Icon: Search },
}

function AlertBadge({ type, resolved }) {
  const { _ } = useLingui()
  const m = ALERT_META[type] ?? { label: type, color: '#8AB8FF', Icon: AlertTriangle }
  return (
    <span style={{
      display: 'inline-flex', alignItems: 'center', gap: 4,
      padding: '2px 8px', borderRadius: 10, fontSize: 11, fontWeight: 600,
      background: resolved ? 'transparent' : m.color + '22',
      border: `1px solid ${resolved ? 'var(--border)' : m.color}`,
      color: resolved ? 'var(--muted)' : m.color,
    }}><m.Icon size={11} /> {resolved ? _('\u2713 Resolved') : _(m.label)}</span>
  )
}

function AlertCard({ alert }) {
  const { _ } = useLingui()
  const d = alert.details
  const meta = ALERT_META[d.type] ?? {}
  const when = new Date(alert.detected_at_ms).toLocaleString()

  return (
    <div style={{
      borderRadius: 8, padding: '14px 16px', marginBottom: 10,
      background: alert.resolved ? '#0f1120' : meta.color ? meta.color + '11' : '#0f1120',
      border: `1px solid ${alert.resolved ? 'var(--border)' : meta.color ?? 'var(--border)'}`,
      opacity: alert.resolved ? 0.55 : 1,
    }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 8, flexWrap: 'wrap' }}>
        <AlertBadge type={d.type} resolved={alert.resolved} />
        <span style={{ fontSize: 11, color: 'var(--muted)' }}>{when}</span>
      </div>

      {/* Fingerprint */}
      <div style={{ marginBottom: 8 }}>
        <code style={{ fontSize: 12, color: '#cdd3de', wordBreak: 'break-all' }}>{alert.fingerprint}</code>
      </div>

      {/* Details row */}
      {d.type === 'latency_regression' && (
        <div style={{ display: 'flex', gap: 24, fontSize: 12, flexWrap: 'wrap' }}>
          <span style={{ color: 'var(--muted)' }}>{_('Baseline p95:')} <strong style={{ color: 'inherit' }}>{(d.baseline_p95_us / 1000).toFixed(2)}ms</strong></span>
          <span style={{ color: 'var(--muted)' }}>{_('Current p95:')} <strong style={{ color: d.ratio >= 2 ? '#F2495C' : '#FFB357' }}>{(d.current_p95_us / 1000).toFixed(2)}ms</strong></span>
          <span style={{ color: '#F2495C', fontWeight: 700 }}>+{((d.ratio - 1) * 100).toFixed(0)}% {_('slower')}</span>
        </div>
      )}
      {d.type === 'hot_key' && (
        <div style={{ fontSize: 12 }}>
          <span style={{ color: 'var(--muted)' }}>{_('Same SQL executed')} </span>
          <strong style={{ color: '#FFB357' }}>{d.hit_count.toLocaleString()}×</strong>
          <span style={{ color: 'var(--muted)' }}> {_('in one session:')} </span>
          <code style={{ fontSize: 11, color: '#cdd3de', wordBreak: 'break-all' }}>{d.example_sql}</code>
        </div>
      )}
      {d.type === 'full_scan_risk' && (
        <div style={{ fontSize: 12, color: 'var(--muted)' }}>
          {d.reason}
          <span style={{ marginLeft: 12, color: '#FF9830', fontWeight: 600 }}>({d.call_count.toLocaleString()} calls)</span>
        </div>
      )}
    </div>
  )
}

function RegressoesPanel() {
  const { _ } = useLingui()
  const { data, loading, error } = useFetch('/api/regressions', 10000)
  const [filter, setFilter] = useState('all') // 'all' | 'latency_regression' | 'hot_key' | 'full_scan_risk' | 'resolved'

  if (loading && !data) return <Spinner />

  const alerts = data ?? []
  const active = alerts.filter(a => !a.resolved)
  const resolved = alerts.filter(a => a.resolved)

  const counts = {
    latency_regression: active.filter(a => a.details.type === 'latency_regression').length,
    hot_key:            active.filter(a => a.details.type === 'hot_key').length,
    full_scan_risk:     active.filter(a => a.details.type === 'full_scan_risk').length,
  }

  const visible = alerts.filter(a => {
    if (filter === 'resolved') return a.resolved
    if (filter === 'all') return !a.resolved
    return !a.resolved && a.details.type === filter
  })

  const filters = [
    { key: 'all',                label: `${_('All active')} (${active.length})` },
    { key: 'latency_regression', label: `${_('Latency')} (${counts.latency_regression})`, color: '#F2495C' },
    { key: 'hot_key',            label: `${_('Hot Key')} (${counts.hot_key})`,            color: '#FFB357' },
    { key: 'full_scan_risk',     label: `${_('Full Scan')} (${counts.full_scan_risk})`,   color: '#FF9830' },
    { key: 'resolved',           label: `${_('Resolved')} (${resolved.length})` },
  ]

  return (
    <div>
      {error && <div style={{ color: 'var(--red)', marginBottom: 12, fontSize: 13 }}>API error: {error}</div>}

      {/* Stat cards */}
      <div className="stat-grid" style={{ marginBottom: 20 }}>
        <StatCard label={_('Active alerts')} value={active.length} sub={_('need attention')} />
        <StatCard label={_('Latency regressions')} value={counts.latency_regression} sub={_('p95 ≤40% above baseline')} />
        <StatCard label={_('Hot keys')} value={counts.hot_key} sub={_('same SQL hammered in session')} />
        <StatCard label={_('Full scan risks')} value={counts.full_scan_risk} sub={_('static analysis')} />
      </div>

      {/* Filter chips */}
      <div style={{ display: 'flex', gap: 6, marginBottom: 16, flexWrap: 'wrap' }}>
        {filters.map(f => (
          <button key={f.key} onClick={() => setFilter(f.key)} style={{
            fontSize: 12, padding: '4px 12px', borderRadius: 12, border: '1px solid',
            borderColor: filter === f.key ? (f.color ?? 'var(--accent)') : 'var(--border)',
            background: filter === f.key ? (f.color ? f.color + '22' : 'var(--accent)') : 'transparent',
            color: filter === f.key ? (f.color ?? '#fff') : 'var(--muted)',
            cursor: 'pointer', fontWeight: filter === f.key ? 700 : 400,
          }}>{f.label}</button>
        ))}
      </div>

      {visible.length === 0 ? (
        <Empty msg="No alerts in this category. The regression checker runs every 5 minutes after a 60-second warm-up." />
      ) : (
        <div>{visible.map(a => <AlertCard key={a.id} alert={a} />)}</div>
      )}
    </div>
  )
}

function LoginPage({ onLogin }) {
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)
  const { _ } = useLingui()

  async function handleSubmit(e) {
    e.preventDefault()
    setLoading(true)
    setError('')
    try {
      const res = await fetch('/api/login', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, password }),
      })
      const json = await res.json()
      if (json.ok && json.token) {
        onLogin(json.token)
      } else {
        setError(json.message || _('Invalid credentials'))
      }
    } catch {
      setError(_('Connection error — is TurbineProxy running?'))
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="login-page">
      <div className="login-left">
        <div className="login-brand">
          <div className="login-logo-mark"><img src={turbineLogo} alt="TurbineProxy" /></div>
          <h1 className="login-title">TurbineProxy</h1>
          <p className="login-tagline">{_('High-performance MySQL & PostgreSQL proxy with real-time observability')}</p>
        </div>
        <div className="login-features">
          <div className="login-feature"><span className="login-feature-dot" />{_('Query analytics & N+1 detection')}</div>
          <div className="login-feature"><span className="login-feature-dot" />{_('Read/write splitting & connection pooling')}</div>
          <div className="login-feature"><span className="login-feature-dot" />{_('Regression alerts & heatmaps')}</div>
        </div>
      </div>
      <div className="login-right">
        <div className="login-toolbar">
          <ThemeToggle />
          <LangSelector />
        </div>
        <form className="login-form" onSubmit={handleSubmit}>
          <div className="login-form-header">
            <Lock size={22} strokeWidth={1.75} />
            <h2>{_('Sign in')}</h2>
            <p>{_('Enter your credentials to access the dashboard')}</p>
          </div>
          <label className="login-label">{_('Username')}</label>
          <input
            className="login-input"
            type="text"
            autoComplete="username"
            placeholder="admin"
            value={username}
            onChange={e => setUsername(e.target.value)}
            required
          />
          <label className="login-label">{_('Password')}</label>
          <input
            className="login-input"
            type="password"
            autoComplete="current-password"
            placeholder="••••••••"
            value={password}
            onChange={e => setPassword(e.target.value)}
            required
          />
          {error && <div className="login-error">{error}</div>}
          <button className="login-btn" type="submit" disabled={loading}>
            {loading ? _('Signing in…') : _('Sign in')}
          </button>
        </form>
      </div>
    </div>
  )
}

export default function App() {
  const [tab, setTab] = useHashTab()
  const [token, setTokenState] = useState(getToken)
  const authed = !!token

  const { data: health } = useFetch('/health', 10000)
  const { data: sideStatsResp } = useFetch(authed ? '/api/stats?protocol=auto' : null, 10000)
  const { data: errorStatsResp } = useFetch(authed ? '/api/errors/stats?protocol=auto' : null, 15000)
  const { data: configStatus } = useFetch(authed ? '/api/config/status' : null, 30000)
  const { data: capabilities } = useFetch(authed ? '/api/capabilities' : null, 20000)
  const dashboardAuthEnabled = capabilities?.dashboard_auth_enabled !== false
  const configModified = configStatus?.modified === true
  const sideStats = sideStatsResp?.data ?? sideStatsResp
  const errorStats = errorStatsResp?.data ?? errorStatsResp
  const { _ } = useLingui()

  const groups = makeNavGroups(_, capabilities)
  const allItems = groups.flatMap(g => g.items)
  const current = allItems.find(it => it.id === tab) ?? allItems[0]

  useEffect(() => {
    if (!allItems.find(it => it.id === tab) && allItems[0]) {
      setTab(allItems[0].id)
    }
  }, [tab, allItems, setTab])

  function handleLogin(t) {
    setToken(t)
    setTokenState(t)
  }

  function handleLogout() {
    const t = getToken()
    if (t) {
      fetch('/api/logout', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', ...authHeaders() },
        body: JSON.stringify({ token: t }),
      }).catch(() => {})
    }
    setToken('')
    setTokenState('')
  }

  const [reloading, setReloading] = useState(false)
  const [reloadMsg, setReloadMsg] = useState(null)
  const [confirmAction, setConfirmAction] = useState(null)

  async function handleReload() {
    setReloading(true)
    setReloadMsg(null)
    try {
      const res = await fetch('/api/reload', {
        method: 'POST',
        headers: { ...authHeaders() },
      })
      const data = await res.json()
      setReloadMsg(data.ok ? 'ok' : 'err')
    } catch {
      setReloadMsg('err')
    } finally {
      setReloading(false)
      setTimeout(() => setReloadMsg(null), 3000)
    }
  }

  const [flushing, setFlushing] = useState(false)
  const [flushMsg, setFlushMsg] = useState(null)
  async function handleFlushStats() {
    setFlushing(true)
    setFlushMsg(null)
    try {
      const res = await fetch('/api/stats/flush', {
        method: 'POST',
        headers: { ...authHeaders() },
      })
      const data = await res.json()
      setFlushMsg(data.ok ? 'ok' : 'err')
    } catch {
      setFlushMsg('err')
    } finally {
      setFlushing(false)
      setTimeout(() => setFlushMsg(null), 3000)
    }
  }

  function requestConfirm(action) {
    setConfirmAction(action)
  }

  async function runConfirmedAction() {
    if (!confirmAction) return
    if (confirmAction === 'reload') {
      await handleReload()
    } else if (confirmAction === 'flush') {
      await handleFlushStats()
    }
    setConfirmAction(null)
  }

  if (!authed) return <LoginPage onLogin={handleLogin} />

  return (
    <div className="layout">
      {/* ── Sidebar ── */}
      <aside className="sidebar">
        <div className="sidebar-logo">
          <div className="sidebar-logo-mark"><img src={turbineLogo} alt="TurbineProxy" /></div>
          <span className="sidebar-logo-name">Turbine<span>Proxy</span></span>
        </div>
        <nav>
          {groups.map(g => (
            <div key={g.id} className="nav-group">
              <div className="nav-group-label">{g.label}</div>
              {g.items.map(item => {
                const errCount = item.id === 'Errors' && (
                  errorStats?.total ?? Object.values(errorStats?.['1h'] || {}).reduce((a, b) => a + b, 0)
                )
                const showModified = item.id === 'Config' && configModified
                return (
                  <button
                    key={item.id}
                    className={`nav-item ${tab === item.id ? 'active' : ''}`}
                    onClick={() => setTab(item.id)}
                  >
                    <span className="nav-item-icon"><item.Icon size={15} strokeWidth={1.75} /></span>
                    {item.label}
                    {errCount > 0 && (
                      <span className="nav-error-badge" title={`${errCount} errors in the last hour`}>{errCount > 99 ? '99+' : errCount}</span>
                    )}
                    {showModified && (
                      <span className="nav-error-badge" style={{ background: '#e67e22' }} title="Config has unsaved changes">●</span>
                    )}
                  </button>
                )
              })}
            </div>
          ))}
        </nav>
        <div className="sidebar-footer">
          {sideStats?.last_reload_secs > 0 && (
            <div className="sidebar-last-reload">
              {_('Config reloaded')} {fmtAgo(sideStats.last_reload_secs)}
            </div>
          )}
          <a
            className="nav-item docs-link"
            href="https://docs.turbineproxy.com/"
            target="_blank"
            rel="noreferrer noopener"
            title="Open TurbineProxy documentation"
          >
            <span className="nav-item-icon"><ExternalLink size={15} strokeWidth={1.75} /></span>
            {_('Documentation')}
          </a>
          <div
            style={{
              borderTop: '1px solid var(--border)',
              margin: '8px 0',
            }}
          />
          <button
            className={`nav-item reload-btn ${reloadMsg === 'ok' ? 'reload-ok' : reloadMsg === 'err' ? 'reload-err' : ''}`}
            onClick={() => requestConfirm('reload')}
            disabled={reloading}
            title={_('Reload config (also works via SIGHUP)')}
          >
            <span className="nav-item-icon"><RotateCw size={15} strokeWidth={1.75} className={reloading ? 'spin' : ''} /></span>
            {_('Reload Config')}
          </button>
          <button
            className={`nav-item reload-btn ${flushMsg === 'ok' ? 'reload-ok' : flushMsg === 'err' ? 'reload-err' : ''}`}
            onClick={() => requestConfirm('flush')}
            disabled={flushing}
            title={_('Reset query counters (queries, errors, blocked) — does not affect connection counts')}
          >
            <span className="nav-item-icon"><RotateCw size={15} strokeWidth={1.75} className={flushing ? 'spin' : ''} /></span>
            {flushMsg === 'ok' ? _('Stats flushed!') : _('Flush Stats')}
          </button>
          {dashboardAuthEnabled && (
            <button className="nav-item logout-btn" onClick={handleLogout}>
              <span className="nav-item-icon"><LogOut size={15} strokeWidth={1.75} /></span>
              {_('Logout')}
            </button>
          )}
          {health?.version && (
            <div className="sidebar-last-reload" style={{ marginTop: 8, textAlign: 'center' }}>
              {`v${health.version}`}
            </div>
          )}
        </div>
      </aside>

      {/* ── Main column ── */}
      <div className="main-col">
        <header className="topbar">
          <span className="topbar-crumb">{current.label}</span>
          {health?.status === 'draining' && (
            <span style={{
              background: '#e67e22',
              color: '#fff',
              borderRadius: 6,
              padding: '3px 10px',
              fontSize: '0.75rem',
              fontWeight: 700,
              letterSpacing: '0.08em',
              marginLeft: 8,
              animation: 'pulse 1.5s ease-in-out infinite',
            }}>
              ⚠ DRAINING
            </span>
          )}
          <span style={{
            display: 'flex',
            alignItems: 'center',
            gap: 6,
            fontSize: '0.78rem',
            fontWeight: 600,
            color: health?.status === 'ok' ? 'var(--green, #4caf50)' : 'var(--red)',
            background: health?.status === 'ok' ? 'rgba(76,175,80,0.1)' : 'rgba(244,67,54,0.1)',
            border: `1px solid ${health?.status === 'ok' ? 'rgba(76,175,80,0.35)' : 'rgba(244,67,54,0.35)'}`,
            borderRadius: 999,
            padding: '4px 10px',
            marginLeft: 'auto',
          }}>
            <span style={{
              width: 7,
              height: 7,
              borderRadius: '50%',
              background: health?.status === 'ok' ? 'var(--green, #4caf50)' : 'var(--red)',
              flexShrink: 0,
            }} />
            {health?.status === 'ok' ? _('online') : _('offline')}
          </span>
          <ThemeToggle />
          <LangSelector />
        </header>
        <main className="main">
          {tab === 'Overview' && <Overview />}
          {tab === 'Queries' && (
            <>
              <div className="section-title">{_('Top queries by count (last 50)')}</div>
              <QueriesTable url="/api/queries" emptyMsg={_('No queries recorded yet.')} />
            </>
          )}
          {tab === 'Slow Queries' && (
            <>
              <div className="section-title">{_('Slowest queries by p95 (last 50)')}</div>
              <QueriesTable url="/api/slow-queries" emptyMsg={_('No slow queries yet.')} />
            </>
          )}
          {tab === 'N+1 Detector' && (
            <>
              <div className="section-title">{_('Repeated query patterns detected per connection')}</div>
              <N1Table />
            </>
          )}
          {tab === 'Connection Pool' && (
            <>
              <div className="section-title">{_('Backend connection pool — multiplexing utilisation')}</div>
              <PoolPanel capabilities={capabilities} />
            </>
          )}
          {tab === 'Backends' && (
            <>
              <div className="section-title">{_('Backend topology — primary, replicas, weights, health and pool counters')}</div>
              <BackendsPanel capabilities={capabilities} />
            </>
          )}
          {tab === 'Cluster' && (
            <>
              <div className="section-title">{_('MySQL Group Replication / InnoDB Cluster — live member topology')}</div>
              <ClusterPanel />
            </>
          )}
          {tab === 'Users' && (
            <>
              <div className="section-title">{_('Connected users — credentials, permissions and live stats')}</div>
              <UsersPanel />
            </>
          )}
          {tab === 'Query Rules' && (
            <>
              <div className="section-title">{_('Configurable query routing rules — hit counts and last match')}</div>
              <QueryRulesPanel />
            </>
          )}
          {tab === 'Rewrite Rules' && (
            <>
              <div className="section-title">{_('Query rewriting rules — alter SQL before dispatch')}</div>
              <RewriteRulesPanel />
            </>
          )}
          {tab === 'Traces' && (
            <>
              <div className="section-title">{_('Transaction traces — full query timeline per BEGIN…COMMIT')}</div>
              <TracesPanel />
            </>
          )}
          {tab === 'Analytics' && (
            <>
              <div className="section-title">{_('Per-user / per-IP / per-app query analytics')}</div>
              <AnalyticsPanel />
            </>
          )}
          {tab === 'Heatmap' && (
            <>
              <div className="section-title">{_('Temporal heatmap — query load by day × hour (UTC)')}</div>
              <HeatmapPanel />
            </>
          )}
          {tab === 'Time-Series' && (
            <>
              <div className="section-title">{_('Query throughput history — persistent time-series with roll-up')}</div>
              <HistoricoPanel />
            </>
          )}
          {tab === 'Regressions' && (
            <>
              <div className="section-title">{_('Active regression alerts — latency spikes, hot keys, and full-scan risks')}</div>
              <RegressoesPanel />
            </>
          )}
          {tab === 'Errors' && (
            <>
              <div className="section-title">{_('Error events — backend errors, blocked queries, and proxy-generated errors')}</div>
              <ErrorsPanel />
            </>
          )}
          {tab === 'Config' && (
            <>
              <div className="section-title">{_('Runtime config management — edit rules, backends and users without restart')}</div>
              <ConfigPanel capabilities={capabilities} />
            </>
          )}
        </main>

        {confirmAction && (
          <div className="confirm-backdrop" onClick={() => setConfirmAction(null)}>
            <div className="confirm-card" onClick={(e) => e.stopPropagation()}>
              <div className="confirm-title">
                {confirmAction === 'reload' ? _('Confirm Reload Config') : _('Confirm Flush Stats')}
              </div>
              <div className="confirm-body">
                {confirmAction === 'reload'
                  ? _('This will reload runtime config and rules now. Continue?')
                  : _('This will reset query/error counters. Continue?')}
              </div>
              <div className="confirm-actions">
                <button
                  className="icon-btn"
                  style={{ width: 'auto', padding: '0 12px' }}
                  onClick={() => setConfirmAction(null)}
                >
                  {_('Cancel')}
                </button>
                <button
                  className="icon-btn"
                  style={{ width: 'auto', padding: '0 12px', background: 'var(--accent)', color: '#fff', borderColor: 'var(--accent)' }}
                  onClick={runConfirmedAction}
                  disabled={reloading || flushing}
                >
                  {confirmAction === 'reload' ? _('Reload Config') : _('Flush Stats')}
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

