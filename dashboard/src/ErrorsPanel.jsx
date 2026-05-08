import { useState, useEffect, useCallback } from 'react'
import { useLingui } from '@lingui/react'
import { AlertCircle, RefreshCw } from 'lucide-react'

function authHeaders() {
  const t = localStorage.getItem('turbineproxy-token') || ''
  return t ? { 'X-Auth-Token': t } : {}
}

// ─── Category badge colours ───────────────────────────────────────────────────
const CAT_COLORS = {
  AUTH:       { bg: 'var(--red)',    text: '#fff' },
  SYNTAX:     { bg: '#e67e22',       text: '#fff' },
  CONNECTION: { bg: '#8e44ad',       text: '#fff' },
  RESOURCE:   { bg: '#c0392b',       text: '#fff' },
  CONSTRAINT: { bg: '#2980b9',       text: '#fff' },
  PROXY:      { bg: 'var(--accent)', text: '#fff' },
  OTHER:      { bg: 'var(--muted)',  text: 'var(--text)' },
}

function CategoryBadge({ cat }) {
  const style = CAT_COLORS[cat] || CAT_COLORS.OTHER
  return (
    <span style={{
      background: style.bg,
      color: style.text,
      borderRadius: 4,
      padding: '2px 7px',
      fontSize: '0.72rem',
      fontWeight: 600,
      letterSpacing: '0.03em',
      whiteSpace: 'nowrap',
    }}>
      {cat}
    </span>
  )
}

function StatBox({ label, value, color }) {
  return (
    <div style={{
      background: 'var(--surface)',
      border: '1px solid var(--border)',
      borderRadius: 10,
      padding: '18px 28px',
      minWidth: 140,
      flex: 1,
    }}>
      <div style={{ fontSize: '2rem', fontWeight: 700, color: color || 'var(--red)' }}>{value}</div>
      <div style={{ fontSize: '0.82rem', color: 'var(--muted)', marginTop: 4 }}>{label}</div>
    </div>
  )
}

// ─── Errors by category (last 1h) bar chart ───────────────────────────────────
function ErrorCategoryChart({ data, t }) {
  if (!data || Object.keys(data).length === 0) return null
  const entries = Object.entries(data).sort((a, b) => b[1] - a[1])
  const max = entries[0][1] || 1
  return (
    <div style={{
      background: 'var(--surface)',
      border: '1px solid var(--border)',
      borderRadius: 8,
      padding: '14px 18px',
      marginBottom: 16,
    }}>
      <div style={{ fontSize: '0.8rem', fontWeight: 600, color: 'var(--text2)', marginBottom: 10 }}>
        {t('Errors by category — last 1h')}
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
        {entries.map(([cat, count]) => {
          const style = CAT_COLORS[cat] || CAT_COLORS.OTHER
          const pct = Math.max(4, Math.round((count / max) * 100))
          return (
            <div key={cat} style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
              <div style={{ width: 80, fontSize: '0.75rem', color: 'var(--text2)', textAlign: 'right', flexShrink: 0 }}>
                {cat}
              </div>
              <div style={{ flex: 1, background: 'var(--surface2)', borderRadius: 4, height: 16, overflow: 'hidden' }}>
                <div style={{
                  width: `${pct}%`,
                  height: '100%',
                  background: style.bg,
                  borderRadius: 4,
                  transition: 'width 0.4s',
                }} />
              </div>
              <div style={{ width: 30, fontSize: '0.78rem', color: 'var(--text)', textAlign: 'right', flexShrink: 0 }}>
                {count}
              </div>
            </div>
          )
        })}
      </div>
    </div>
  )
}

// ─── Top 10 error codes ────────────────────────────────────────────────────────
function TopErrorCodes({ codes, t }) {
  if (!codes || codes.length === 0) return null
  return (
    <div style={{
      background: 'var(--surface)',
      border: '1px solid var(--border)',
      borderRadius: 8,
      padding: '14px 18px',
      marginBottom: 16,
    }}>
      <div style={{ fontSize: '0.8rem', fontWeight: 600, color: 'var(--text2)', marginBottom: 10 }}>
        {t('Top error codes — last 24h')}
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
        {codes.map((item, i) => (
          <div key={item.code} style={{ display: 'flex', alignItems: 'center', gap: 10, fontSize: '0.83rem' }}>
            <div style={{ width: 20, color: 'var(--muted)', textAlign: 'right', flexShrink: 0 }}>
              {i + 1}.
            </div>
            <code style={{ color: 'var(--red)', fontWeight: 600, minWidth: 42 }}>{item.code}</code>
            <CategoryBadge cat={item.category} />
            <div style={{ flex: 1 }} />
            <div style={{
              background: 'var(--surface2)',
              border: '1px solid var(--border)',
              borderRadius: 10,
              padding: '1px 9px',
              fontSize: '0.78rem',
              fontWeight: 600,
              color: 'var(--text)',
            }}>
              {item.count}
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

function ts(unix) {
  const d = new Date(unix * 1000)
  return d.toLocaleString()
}

// ─── ErrorsPanel ──────────────────────────────────────────────────────────────
export function ErrorsPanel() {
  const { _ } = useLingui()

  const [events, setEvents]   = useState([])
  const [stats, setStats]     = useState(null)
  const [loading, setLoading] = useState(true)
  const [catFilter, setCat]   = useState('')
  const [expanded, setExpanded] = useState(null)

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const [evRes, stRes] = await Promise.all([
        fetch(`/api/errors?protocol=auto&limit=200${catFilter ? `&category=${catFilter}` : ''}`, { headers: authHeaders() }),
        fetch('/api/errors/stats?protocol=auto', { headers: authHeaders() }),
      ])
      const evData  = evRes.ok  ? await evRes.json()  : { events: [] }
      const stData  = stRes.ok  ? await stRes.json()  : {}
      setEvents(evData.events || [])
      setStats(stData?.data ?? stData)
    } finally {
      setLoading(false)
    }
  }, [catFilter])

  useEffect(() => { load() }, [load])
  useEffect(() => {
    const id = setInterval(load, 15000)
    return () => clearInterval(id)
  }, [load])

  // ── Stats summary bar
  const total1h  = stats ? Object.values(stats['1h']  || {}).reduce((a, b) => a + b, 0) : '—'
  const total24h = stats ? Object.values(stats['24h'] || {}).reduce((a, b) => a + b, 0) : '—'
  const total7d  = stats ? Object.values(stats['7d']  || {}).reduce((a, b) => a + b, 0) : '—'
  const totalAll = stats?.total ?? '—'

  const categories = ['', 'AUTH', 'SYNTAX', 'CONNECTION', 'RESOURCE', 'CONSTRAINT', 'PROXY', 'OTHER']

  return (
    <div style={{ padding: '0 4px' }}>
      {/* Stats row */}
      <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', marginBottom: 20 }}>
        <StatBox label={_('Last 1h')}  value={total1h}  />
        <StatBox label={_('Last 24h')} value={total24h} color="var(--accent)" />
        <StatBox label={_('Last 7d')}  value={total7d}  color="var(--muted)" />
        <StatBox label={_('All-time')} value={totalAll} color="var(--text2)" />
      </div>

      {/* Charts row */}
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 14, marginBottom: 4 }}>
        <ErrorCategoryChart data={stats?.['1h']} t={_} />
        <TopErrorCodes codes={stats?.top_codes} t={_} />
      </div>

      {/* Toolbar */}
      <div style={{ display: 'flex', gap: 10, alignItems: 'center', marginBottom: 14, flexWrap: 'wrap' }}>
        <select
          value={catFilter}
          onChange={e => setCat(e.target.value)}
          style={{
            background: 'var(--surface2)',
            border: '1px solid var(--border)',
            borderRadius: 6,
            color: 'var(--text)',
            padding: '6px 10px',
            fontSize: '0.84rem',
          }}
        >
          {categories.map(c => (
            <option key={c} value={c}>{c || _('All categories')}</option>
          ))}
        </select>

        <button
          onClick={load}
          style={{
            background: 'var(--surface)',
            border: '1px solid var(--border)',
            borderRadius: 6,
            color: 'var(--text)',
            padding: '6px 12px',
            cursor: 'pointer',
            display: 'flex', alignItems: 'center', gap: 6,
            fontSize: '0.84rem',
          }}
        >
          <RefreshCw size={14} /> {_('Refresh')}
        </button>

        <span style={{ color: 'var(--muted)', fontSize: '0.82rem' }}>
          {loading ? _('Loading…') : `${events.length} ${_('events')}`}
        </span>
      </div>

      {/* Table */}
      {events.length === 0 ? (
        <div style={{
          background: 'var(--surface)',
          border: '1px solid var(--border)',
          borderRadius: 8,
          padding: '40px 24px',
          textAlign: 'center',
          color: 'var(--muted)',
        }}>
          <AlertCircle size={32} style={{ marginBottom: 10, opacity: 0.4 }} />
          <div style={{ fontSize: '1rem' }}>{_('No error events recorded')}</div>
          <div style={{ fontSize: '0.82rem', marginTop: 4 }}>{_('Events appear here when backend or proxy errors occur')}</div>
        </div>
      ) : (
        <div style={{
          background: 'var(--surface)',
          border: '1px solid var(--border)',
          borderRadius: 8,
          overflow: 'hidden',
        }}>
          <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: '0.84rem' }}>
            <thead>
              <tr style={{ background: 'var(--surface2)', color: 'var(--text2)' }}>
                <th style={{ padding: '9px 12px', textAlign: 'left', fontWeight: 600 }}>{_('Time')}</th>
                <th style={{ padding: '9px 12px', textAlign: 'left', fontWeight: 600 }}>{_('Category')}</th>
                <th style={{ padding: '9px 12px', textAlign: 'left', fontWeight: 600 }}>{_('Code')}</th>
                <th style={{ padding: '9px 12px', textAlign: 'left', fontWeight: 600 }}>{_('Message')}</th>
                <th style={{ padding: '9px 12px', textAlign: 'left', fontWeight: 600 }}>{_('User')}</th>
                <th style={{ padding: '9px 12px', textAlign: 'left', fontWeight: 600 }}>{_('Client IP')}</th>
                <th style={{ padding: '9px 12px', textAlign: 'right', fontWeight: 600 }}>{_('Duration')}</th>
              </tr>
            </thead>
            <tbody>
              {events.map((ev, i) => {
                const isExp = expanded === i
                return (
                  <>
                    <tr
                      key={i}
                      onClick={() => setExpanded(isExp ? null : i)}
                      style={{
                        borderTop: '1px solid var(--border)',
                        cursor: 'pointer',
                        background: isExp ? 'var(--surface2)' : undefined,
                      }}
                    >
                      <td style={{ padding: '8px 12px', color: 'var(--text2)', whiteSpace: 'nowrap' }}>
                        {ts(ev.ts)}
                      </td>
                      <td style={{ padding: '8px 12px' }}>
                        <CategoryBadge cat={ev.category} />
                      </td>
                      <td style={{ padding: '8px 12px', fontFamily: 'monospace', color: 'var(--red)' }}>
                        {ev.code || '—'}
                      </td>
                      <td style={{
                        padding: '8px 12px',
                        maxWidth: 320,
                        overflow: 'hidden',
                        textOverflow: 'ellipsis',
                        whiteSpace: 'nowrap',
                        color: 'var(--text)',
                      }}>
                        {ev.message}
                      </td>
                      <td style={{ padding: '8px 12px', color: 'var(--text2)' }}>{ev.user || '—'}</td>
                      <td style={{ padding: '8px 12px', color: 'var(--text2)', fontFamily: 'monospace', fontSize: '0.8rem' }}>
                        {ev.client_ip || '—'}
                      </td>
                      <td style={{ padding: '8px 12px', textAlign: 'right', color: 'var(--text2)', fontFamily: 'monospace', fontSize: '0.8rem' }}>
                        {ev.duration_ms > 0 ? `${ev.duration_ms.toFixed(1)}ms` : '—'}
                      </td>
                    </tr>
                    {isExp && (
                      <tr key={`${i}-exp`} style={{ background: 'var(--surface2)' }}>
                        <td colSpan={7} style={{ padding: '10px 16px' }}>
                          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10, fontSize: '0.82rem' }}>
                            <div>
                              <span style={{ color: 'var(--muted)', marginRight: 6 }}>{_('Fingerprint:')}</span>
                              <span style={{ fontFamily: 'monospace', color: 'var(--accent)' }}>{ev.fingerprint || '—'}</span>
                            </div>
                            <div>
                              <span style={{ color: 'var(--muted)', marginRight: 6 }}>{_('Backend:')}</span>
                              <span style={{ fontFamily: 'monospace' }}>{ev.backend_addr || '—'}</span>
                            </div>
                            <div style={{ gridColumn: '1 / -1' }}>
                              <span style={{ color: 'var(--muted)', marginRight: 6 }}>{_('Full message:')}</span>
                              <span style={{ color: 'var(--text)' }}>{ev.message}</span>
                            </div>
                          </div>
                        </td>
                      </tr>
                    )}
                  </>
                )
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  )
}
