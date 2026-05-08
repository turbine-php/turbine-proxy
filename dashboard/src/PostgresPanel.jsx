/**
 * PostgresPanel.jsx — Phase 2 PostgreSQL proxy dashboard panel.
 *
 * Shows the PostgreSQL connection pool stats and per-backend health (primary + replicas).
 * Polls GET /api/pool?protocol=pgsql every 5 seconds.
 */

import { useState, useEffect } from 'react'
import { Database, CheckCircle, AlertTriangle, Activity, XCircle, Clock, AlertCircle } from 'lucide-react'

// ─── Simple fetch hook ────────────────────────────────────────────────────────

function usePgPool(interval = 5000) {
  const [data,    setData]    = useState(null)
  const [loading, setLoading] = useState(true)
  const [error,   setError]   = useState(null)

  useEffect(() => {
    let alive = true
    async function fetch_() {
      try {
        const res = await fetch('/api/pool?protocol=pgsql', {
          headers: { 'X-Auth-Token': localStorage.getItem('turbineproxy-token') || '' },
        })
        if (!res.ok) throw new Error(`HTTP ${res.status}`)
        const json = await res.json()
        if (alive) { setData(json); setError(null) }
      } catch (e) {
        if (alive) setError(e.message)
      } finally {
        if (alive) setLoading(false)
      }
    }
    fetch_()
    const id = setInterval(fetch_, interval)
    return () => { alive = false; clearInterval(id) }
  }, [interval])

  return { data, loading, error }
}

// ─── Stat card ────────────────────────────────────────────────────────────────

function StatCard({ label, value, sub, color }) {
  return (
    <div style={{
      background: 'var(--card)', border: '1px solid var(--border)',
      borderRadius: 8, padding: '14px 18px', minWidth: 120,
    }}>
      <div style={{ fontSize: 11, color: 'var(--text-muted)', marginBottom: 4 }}>{label}</div>
      <div style={{ fontSize: 22, fontWeight: 700, color: color || 'var(--text)' }}>{value}</div>
      {sub && <div style={{ fontSize: 11, color: 'var(--text-muted)', marginTop: 2 }}>{sub}</div>}
    </div>
  )
}

// ─── Pool detail table ────────────────────────────────────────────────────────

function PoolTable({ title, idle, inUse, created, reused, evicted }) {
  const hitRate = created > 0 ? ((reused / (reused + created)) * 100).toFixed(1) : '—'
  return (
    <div style={{
      background: 'var(--card)', border: '1px solid var(--border)',
      borderRadius: 8, padding: 16, flex: 1, minWidth: 220,
    }}>
      <div style={{ fontWeight: 600, marginBottom: 12, display: 'flex', alignItems: 'center', gap: 6 }}>
        <Activity size={14} style={{ color: 'var(--accent)' }} />
        {title}
      </div>
      <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
        <tbody>
          {[
            ['Idle connections',       idle],
            ['In-use connections',     inUse],
            ['Total created',          created],
            ['Total reused',           reused],
            ['Evicted (idle timeout)', evicted],
            ['Pool hit rate',          hitRate === '—' ? '—' : `${hitRate} %`],
          ].map(([label, val]) => (
            <tr key={label} style={{ borderBottom: '1px solid var(--border)' }}>
              <td style={{ padding: '6px 0', color: 'var(--text-muted)' }}>{label}</td>
              <td style={{ padding: '6px 0', fontWeight: 500, textAlign: 'right' }}>{val}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

// ─── Backend health table ─────────────────────────────────────────────────────

function BackendHealthTable({ backends, failoverActive }) {
  if (!backends || backends.length === 0) return null
  return (
    <div style={{
      background: 'var(--card)', border: '1px solid var(--border)',
      borderRadius: 8, overflow: 'hidden',
    }}>
      <div style={{
        padding: '10px 16px', borderBottom: '1px solid var(--border)',
        fontWeight: 600, fontSize: 13, display: 'flex', alignItems: 'center', gap: 8,
      }}>
        <Database size={14} style={{ color: 'var(--accent)' }} />
        Backend Health
        {failoverActive && (
          <span style={{
            background: '#e67e22', color: '#fff',
            padding: '2px 8px', borderRadius: 4, fontSize: 11, fontWeight: 700,
          }}>FAILOVER ACTIVE</span>
        )}
      </div>
      <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
        <thead>
          <tr style={{ background: 'var(--bg)', borderBottom: '1px solid var(--border)' }}>
            {['Address', 'Role', 'Status', 'Lag', 'Idle', 'In Use', 'Created', 'Reused', 'Failures'].map(h => (
              <th key={h} style={{ padding: '8px 12px', textAlign: 'left', fontWeight: 600, color: 'var(--text-muted)', fontSize: 11 }}>{h}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {backends.map((b, i) => (
            <tr key={i} style={{ borderBottom: '1px solid var(--border)' }}>
              <td style={{ padding: '8px 12px', fontFamily: 'monospace', fontSize: 12 }}>{b.addr}</td>
              <td style={{ padding: '8px 12px' }}>
                <span style={{
                  background: b.role === 'primary' ? '#2980b9' : '#27ae60',
                  color: '#fff', padding: '2px 7px', borderRadius: 4, fontSize: 11, fontWeight: 600,
                }}>{b.role}</span>
                {b.backup && <span style={{ marginLeft: 4, color: 'var(--text-muted)', fontSize: 11 }}>backup</span>}
              </td>
              <td style={{ padding: '8px 12px' }}>
                {b.healthy
                  ? <span style={{ color: '#2ecc71', display: 'flex', alignItems: 'center', gap: 4 }}><CheckCircle size={13} /> healthy</span>
                  : <span style={{ color: '#e74c3c', display: 'flex', alignItems: 'center', gap: 4 }}><XCircle size={13} /> unhealthy</span>
                }
              </td>
              <td style={{ padding: '8px 12px', color: b.lag_ms > 2000 ? '#e74c3c' : b.lag_ms > 500 ? '#e67e22' : 'inherit' }}>
                {b.role === 'primary' ? '—' : b.lag_ms > 0 ? `${b.lag_ms} ms` : '0 ms'}
              </td>
              <td style={{ padding: '8px 12px' }}>{b.idle}</td>
              <td style={{ padding: '8px 12px' }}>{b.in_use}</td>
              <td style={{ padding: '8px 12px' }}>{b.created}</td>
              <td style={{ padding: '8px 12px' }}>{b.reused}</td>
              <td style={{ padding: '8px 12px', color: b.consecutive_failures > 0 ? '#e74c3c' : 'inherit' }}>
                {b.consecutive_failures}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

// ─── Main panel ──────────────────────────────────────────────────────────────

function usePgSlowQueries(interval = 15000) {
  const [data, setData] = useState(null)
  useEffect(() => {
    let alive = true
    async function fetch_() {
      try {
        const r = await fetch('/api/slow-queries?protocol=pgsql&limit=20')
        if (alive && r.ok) setData(await r.json())
      } catch (_err) {
        // ignore transient polling errors
      }
    }
    fetch_()
    const id = setInterval(fetch_, interval)
    return () => { alive = false; clearInterval(id) }
  }, [interval])
  return data
}

function usePgErrors(interval = 10000) {
  const [data, setData] = useState(null)
  useEffect(() => {
    let alive = true
    async function fetch_() {
      try {
        const r = await fetch('/api/errors?protocol=postgres&limit=20')
        if (alive && r.ok) setData(await r.json())
      } catch (_err) {
        // ignore transient polling errors
      }
    }
    fetch_()
    const id = setInterval(fetch_, interval)
    return () => { alive = false; clearInterval(id) }
  }, [interval])
  return data
}

function SlowQueriesSection() {
  const data = usePgSlowQueries()
  const rows = data?.data ?? data?.slow_queries ?? []
  return (
    <div style={{ background: 'var(--card)', border: '1px solid var(--border)', borderRadius: 8, overflow: 'hidden' }}>
      <div style={{ padding: '10px 14px', fontWeight: 600, fontSize: 13, borderBottom: '1px solid var(--border)', display: 'flex', alignItems: 'center', gap: 6 }}>
        <Clock size={14} /> PostgreSQL Slow Queries
      </div>
      {rows.length === 0
        ? <div style={{ padding: '12px 14px', color: 'var(--text-muted)', fontSize: 13 }}>No slow queries recorded.</div>
        : (
          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 12 }}>
            <thead>
              <tr style={{ background: 'var(--bg)', color: 'var(--text-muted)', textAlign: 'left' }}>
                <th style={{ padding: '6px 12px' }}>Fingerprint</th>
                <th style={{ padding: '6px 12px' }}>Count</th>
                <th style={{ padding: '6px 12px' }}>Avg ms</th>
                <th style={{ padding: '6px 12px' }}>p95 ms</th>
                <th style={{ padding: '6px 12px' }}>p99 ms</th>
                <th style={{ padding: '6px 12px' }}>Last seen</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((q, i) => (
                <tr key={i} style={{ borderTop: '1px solid var(--border)' }}>
                  <td style={{ padding: '6px 12px', fontFamily: 'monospace', maxWidth: 320, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{q.fingerprint}</td>
                  <td style={{ padding: '6px 12px' }}>{q.count}</td>
                  <td style={{ padding: '6px 12px' }}>{q.avg_ms ?? Math.round((q.total_us ?? 0) / 1000 / (q.count || 1))}</td>
                  <td style={{ padding: '6px 12px' }}>{q.p95_ms ?? '—'}</td>
                  <td style={{ padding: '6px 12px' }}>{q.p99_ms ?? '—'}</td>
                  <td style={{ padding: '6px 12px', color: 'var(--text-muted)' }}>{q.last_seen ? new Date(q.last_seen).toLocaleString() : '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )
      }
    </div>
  )
}

function PgErrorsSection() {
  const data = usePgErrors()
  const rows = data?.events ?? []
  return (
    <div style={{ background: 'var(--card)', border: '1px solid var(--border)', borderRadius: 8, overflow: 'hidden' }}>
      <div style={{ padding: '10px 14px', fontWeight: 600, fontSize: 13, borderBottom: '1px solid var(--border)', display: 'flex', alignItems: 'center', gap: 6 }}>
        <AlertCircle size={14} /> Recent PostgreSQL Errors
      </div>
      {rows.length === 0
        ? <div style={{ padding: '12px 14px', color: 'var(--text-muted)', fontSize: 13 }}>No errors recorded.</div>
        : (
          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 12 }}>
            <thead>
              <tr style={{ background: 'var(--bg)', color: 'var(--text-muted)', textAlign: 'left' }}>
                <th style={{ padding: '6px 12px' }}>Time</th>
                <th style={{ padding: '6px 12px' }}>User</th>
                <th style={{ padding: '6px 12px' }}>Client</th>
                <th style={{ padding: '6px 12px' }}>Message</th>
                <th style={{ padding: '6px 12px' }}>ms</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((e, i) => (
                <tr key={i} style={{ borderTop: '1px solid var(--border)' }}>
                  <td style={{ padding: '6px 12px', color: 'var(--text-muted)', whiteSpace: 'nowrap' }}>{new Date(e.timestamp ?? e.ts ?? '').toLocaleString()}</td>
                  <td style={{ padding: '6px 12px' }}>{e.user ?? '—'}</td>
                  <td style={{ padding: '6px 12px', fontFamily: 'monospace' }}>{e.client_ip ?? '—'}</td>
                  <td style={{ padding: '6px 12px', color: '#e74c3c', maxWidth: 300, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{e.message ?? e.error ?? '—'}</td>
                  <td style={{ padding: '6px 12px' }}>{e.duration_ms ?? '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )
      }
    </div>
  )
}

export function PostgresPanel() {
  const { data, loading, error } = usePgPool()

  if (loading) {
    return (
      <div style={{ padding: 24, color: 'var(--text-muted)' }}>
        Loading PostgreSQL pool stats…
      </div>
    )
  }

  if (error) {
    return (
      <div style={{ padding: 24, color: '#e74c3c', display: 'flex', alignItems: 'center', gap: 8 }}>
        <AlertTriangle size={16} />
        Failed to load PostgreSQL stats: {error}
      </div>
    )
  }

  if (!data?.enabled) {
    return (
      <div style={{
        padding: 32, textAlign: 'center', color: 'var(--text-muted)',
        display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 12,
      }}>
        <Database size={32} style={{ opacity: 0.4 }} />
        <div style={{ fontSize: 16, fontWeight: 600 }}>PostgreSQL proxy is disabled</div>
        <div style={{ fontSize: 13 }}>
          Add to your <code>turbineproxy.toml</code>:
          <pre style={{
            background: 'var(--card)', border: '1px solid var(--border)',
            borderRadius: 6, padding: '10px 14px', marginTop: 8, textAlign: 'left',
            fontSize: 12, lineHeight: 1.6,
          }}>
{`[pgsql]
enabled = true
listen_addr = "0.0.0.0:5433"

[pgsql.primary]
addr = "127.0.0.1:5432"
user = "postgres"
password = "secret"
database = "mydb"`}
          </pre>
        </div>
      </div>
    )
  }

  const p = data.pool || {}
  const backends = data.backends || []
  const failoverActive = p.failover_active || false

  const primaryTotal = (p.primary_idle || 0) + (p.primary_in_use || 0)
  const replicaTotal  = (p.replica_idle  || 0) + (p.replica_in_use  || 0)
  const primaryHealthy = backends.find(b => b.role === 'primary')?.healthy !== false
  const unhealthyCount = backends.filter(b => !b.healthy).length

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 16 }}>

      {/* Status bar */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, fontSize: 13, flexWrap: 'wrap' }}>
        {primaryHealthy
          ? <><CheckCircle size={14} style={{ color: '#2ecc71' }} /><span style={{ color: '#2ecc71', fontWeight: 600 }}>PostgreSQL proxy active</span></>
          : <><XCircle size={14} style={{ color: '#e74c3c' }} /><span style={{ color: '#e74c3c', fontWeight: 600 }}>Primary backend unreachable</span></>
        }
        {unhealthyCount > 0 && (
          <span style={{ color: '#e67e22', fontWeight: 500 }}>
            {unhealthyCount} backend{unhealthyCount > 1 ? 's' : ''} unhealthy
          </span>
        )}
        {failoverActive && (
          <span style={{
            background: '#e67e22', color: '#fff',
            padding: '2px 8px', borderRadius: 4, fontSize: 11, fontWeight: 700,
          }}>⚠ FAILOVER ACTIVE</span>
        )}
      </div>

      {/* Summary cards */}
      <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap' }}>
        <StatCard label="Primary connections"   value={primaryTotal} />
        <StatCard label="Primary idle"          value={p.primary_idle  ?? 0} color="#2ecc71" />
        <StatCard label="Primary in use"        value={p.primary_in_use ?? 0} color="#e67e22" />
        {(p.replica_count ?? 0) > 0 && <>
          <StatCard label="Replica count"       value={p.replica_count ?? 0} />
          <StatCard label="Replica connections" value={replicaTotal} />
          <StatCard label="Replica idle"        value={p.replica_idle  ?? 0} color="#2ecc71" />
          <StatCard label="Replica in use"      value={p.replica_in_use ?? 0} color="#e67e22" />
        </>}
      </div>

      {/* Backend health table */}
      <BackendHealthTable backends={backends} failoverActive={failoverActive} />

      {/* Pool detail tables */}
      <div style={{ display: 'flex', gap: 16, flexWrap: 'wrap' }}>
        <PoolTable
          title="Primary backend pool"
          idle={p.primary_idle     ?? 0}
          inUse={p.primary_in_use  ?? 0}
          created={p.primary_created ?? 0}
          reused={p.primary_reused  ?? 0}
          evicted={p.primary_evicted ?? 0}
        />
        {(p.replica_count ?? 0) > 0 && (
          <PoolTable
            title={`Replica pool (${p.replica_count} backend${p.replica_count === 1 ? '' : 's'})`}
            idle={p.replica_idle    ?? 0}
            inUse={p.replica_in_use ?? 0}
            created={p.replica_created ?? 0}
            reused={p.replica_reused  ?? 0}
            evicted={p.replica_evicted ?? 0}
          />
        )}
      </div>

      <div style={{ fontSize: 11, color: 'var(--text-muted)' }}>
        Auto-refreshes every 5 s
      </div>

      <SlowQueriesSection />
      <PgErrorsSection />
    </div>
  )
}
