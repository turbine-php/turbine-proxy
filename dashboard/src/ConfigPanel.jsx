// ─── ConfigPanel — Runtime Config Management (Fase 0.5) ───────────────────────
// Manages query rules, rewrite rules, backends, users, and config history
// via /api/config/* endpoints.  All mutations hot-reload the proxy in-process.

import { useState, useEffect, useCallback, useRef } from 'react'
import { useLingui } from '@lingui/react'
import {
  Filter, Pencil, Server, Users as UsersIcon,
  PlusCircle, Trash2, Edit2, Check, X, Clock, Download, Upload,
} from 'lucide-react'

function authHeaders() {
  const t = localStorage.getItem('turbineproxy-token') || ''
  return t ? { 'X-Auth-Token': t, 'Content-Type': 'application/json' }
           : { 'Content-Type': 'application/json' }
}

async function apiFetch(url, opts = {}) {
  const res = await fetch(url, { headers: authHeaders(), ...opts })
  if (!res.ok) {
    const body = await res.text().catch(() => res.statusText)
    throw new Error(body || res.statusText)
  }
  return res.json()
}

// ─── tiny reusable input ─────────────────────────────────────────────────────
function Field({ label, value, onChange, type = 'text', placeholder = '' }) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 3 }}>
      <label style={{ fontSize: 11, color: 'var(--subtext)', textTransform: 'uppercase', letterSpacing: '.04em' }}>
        {label}
      </label>
      <input
        type={type}
        value={value}
        placeholder={placeholder}
        onChange={e => onChange(e.target.value)}
        style={{
          background: 'var(--input-bg, var(--surface2))',
          border: '1px solid var(--border)',
          borderRadius: 6, padding: '6px 10px',
          color: 'var(--text)', fontSize: 13,
        }}
      />
    </div>
  )
}

function Select({ label, value, onChange, options }) {
  const { _ } = useLingui()
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 3 }}>
      <label style={{ fontSize: 11, color: 'var(--subtext)', textTransform: 'uppercase', letterSpacing: '.04em' }}>
        {label}
      </label>
      <select
        value={value}
        onChange={e => onChange(e.target.value)}
        style={{
          background: 'var(--input-bg, var(--surface2))',
          border: '1px solid var(--border)',
          borderRadius: 6, padding: '6px 10px',
          color: 'var(--text)', fontSize: 13,
        }}
      >
        {options.map(o => (
          <option key={o.value} value={o.value}>{_(o.label)}</option>
        ))}
      </select>
    </div>
  )
}

function Toggle({ label, value, onChange }) {
  return (
    <label style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer', userSelect: 'none' }}>
      <div
        onClick={() => onChange(!value)}
        style={{
          width: 36, height: 20, borderRadius: 10,
          background: value ? 'var(--accent)' : 'var(--border)',
          position: 'relative', transition: 'background .2s',
          cursor: 'pointer',
        }}
      >
        <div style={{
          width: 16, height: 16, borderRadius: 8, background: '#fff',
          position: 'absolute', top: 2, left: value ? 18 : 2,
          transition: 'left .2s',
          boxShadow: '0 1px 3px rgba(0,0,0,.3)',
        }} />
      </div>
      <span style={{ fontSize: 13 }}>{label}</span>
    </label>
  )
}

function ErrMsg({ err }) {
  if (!err) return null
  return <div style={{ color: 'var(--red)', fontSize: 12, marginTop: 6 }}>{err}</div>
}

// ─── sub-nav pill strip ───────────────────────────────────────────────────────
function SubNav({ tabs, active, setActive }) {
  return (
    <div style={{ display: 'flex', gap: 6, marginBottom: 20, flexWrap: 'wrap' }}>
      {tabs.map(t => (
        <button
          key={t.key}
          onClick={() => setActive(t.key)}
          style={{
            display: 'flex', alignItems: 'center', gap: 6,
            fontSize: 12, padding: '5px 14px', borderRadius: 20,
            border: '1px solid',
            borderColor: active === t.key ? 'var(--accent)' : 'var(--border)',
            background: active === t.key ? 'var(--accent)' : 'transparent',
            color: active === t.key ? '#fff' : 'var(--text)',
            cursor: 'pointer', fontWeight: 600, transition: 'all .15s',
          }}
        >
          {t.Icon && <t.Icon size={13} />}
          {t.label}
        </button>
      ))}
    </div>
  )
}

// ─── Query Rules ──────────────────────────────────────────────────────────────
const DESTINATIONS = [
  { value: 'primary',  label: 'Primary' },
  { value: 'replica',  label: 'Replica' },
  { value: 'any',      label: 'Any' },
]

const ACTIONS = [
  { value: 'route',    label: 'Route' },
  { value: 'block',    label: 'Block' },
  { value: 'log',      label: 'Log only' },
]

function emptyRule() {
  return { pattern: '', destination: 'replica', action: 'route', priority: 100, enabled: true }
}

function QueryRulesConfig() {
  const { _ } = useLingui()
  const [rows, setRows] = useState([])
  const [loading, setLoading] = useState(true)
  const [err, setErr] = useState(null)
  const [adding, setAdding] = useState(false)
  const [form, setForm] = useState(emptyRule())
  const [editing, setEditing] = useState(null)   // id being edited
  const [editForm, setEditForm] = useState(null)

  const load = useCallback(async () => {
    try {
      setLoading(true)
      const d = await apiFetch('/api/config/rules')
      setRows(d)
      setErr(null)
    } catch (e) { setErr(e.message) }
    finally { setLoading(false) }
  }, [])

  useEffect(() => { load() }, [load])

  const save = async () => {
    try {
      await apiFetch('/api/config/rules', {
        method: 'POST',
        body: JSON.stringify({ ...form, priority: Number(form.priority) }),
      })
      setAdding(false)
      setForm(emptyRule())
      load()
    } catch (e) { setErr(e.message) }
  }

  const update = async (id) => {
    try {
      await apiFetch(`/api/config/rules/${id}`, {
        method: 'PUT',
        body: JSON.stringify({ ...editForm, priority: Number(editForm.priority) }),
      })
      setEditing(null)
      load()
    } catch (e) { setErr(e.message) }
  }

  const remove = async (id) => {
    if (!window.confirm(_('Delete this rule?'))) return
    try {
      await apiFetch(`/api/config/rules/${id}`, { method: 'DELETE' })
      load()
    } catch (e) { setErr(e.message) }
  }

  const startEdit = (row) => { setEditing(row.id); setEditForm({ ...row }) }

  if (loading) return <div style={{ color: 'var(--subtext)', fontSize: 13 }}>{_('Loading…')}</div>

  return (
    <div>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 12 }}>
        <span style={{ fontSize: 13, color: 'var(--subtext)' }}>
          {rows.length} {rows.length !== 1 ? _('rules') : _('rule')} — {_('changes apply immediately')}
        </span>
        <button
          onClick={() => setAdding(true)}
          style={{
            display: 'flex', alignItems: 'center', gap: 6,
            background: 'var(--accent)', color: '#fff', border: 'none',
            borderRadius: 8, padding: '7px 14px', cursor: 'pointer', fontSize: 13, fontWeight: 600,
          }}
        >
          <PlusCircle size={14} /> {_('Add rule')}
        </button>
      </div>
      <ErrMsg err={err} />

      {/* add form */}
      {adding && (
        <div style={{ background: 'var(--surface)', border: '1px solid var(--accent)', borderRadius: 10, padding: 16, marginBottom: 14 }}>
          <div style={{ display: 'grid', gridTemplateColumns: '2fr 1fr 1fr 80px', gap: 10, marginBottom: 10 }}>
            <Field label={_('Pattern (regex)')} value={form.pattern} onChange={v => setForm(f => ({ ...f, pattern: v }))} placeholder="^SELECT" />
            <Select label={_('Destination')} value={form.destination} onChange={v => setForm(f => ({ ...f, destination: v }))} options={DESTINATIONS} />
            <Select label={_('Action')} value={form.action} onChange={v => setForm(f => ({ ...f, action: v }))} options={ACTIONS} />
            <Field label={_('Priority')} value={form.priority} onChange={v => setForm(f => ({ ...f, priority: v }))} type="number" />
          </div>
          <Toggle label={_('Enabled')} value={form.enabled} onChange={v => setForm(f => ({ ...f, enabled: v }))} />
          <div style={{ display: 'flex', gap: 8, marginTop: 12 }}>
            <button onClick={save} style={{ background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 7, padding: '6px 16px', cursor: 'pointer', fontWeight: 600, fontSize: 13 }}>{_('Save')}</button>
            <button onClick={() => { setAdding(false); setErr(null) }} style={{ background: 'transparent', color: 'var(--subtext)', border: '1px solid var(--border)', borderRadius: 7, padding: '6px 14px', cursor: 'pointer', fontSize: 13 }}>{_('Cancel')}</button>
          </div>
        </div>
      )}

      {/* table */}
      <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
        <thead>
          <tr style={{ borderBottom: '1px solid var(--border)', color: 'var(--subtext)', fontSize: 11, textTransform: 'uppercase' }}>
            <th style={{ textAlign: 'left', padding: '6px 8px' }}>{_('Pattern')}</th>
            <th style={{ textAlign: 'left', padding: '6px 8px' }}>{_('Dest')}</th>
            <th style={{ textAlign: 'left', padding: '6px 8px' }}>{_('Action')}</th>
            <th style={{ textAlign: 'right', padding: '6px 8px' }}>{_('Priority')}</th>
            <th style={{ textAlign: 'center', padding: '6px 8px' }}>{_('Enabled')}</th>
            <th style={{ textAlign: 'right', padding: '6px 8px' }}></th>
          </tr>
        </thead>
        <tbody>
          {rows.map(row => editing === row.id ? (
            <tr key={row.id} style={{ background: 'var(--surface2)' }}>
              <td style={{ padding: '6px 8px' }}><input value={editForm.pattern} onChange={e => setEditForm(f => ({ ...f, pattern: e.target.value }))} style={{ width: '100%', background: 'var(--input-bg, var(--surface2))', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 8px', color: 'var(--text)', fontSize: 13 }} /></td>
              <td style={{ padding: '6px 8px' }}><select value={editForm.destination} onChange={e => setEditForm(f => ({ ...f, destination: e.target.value }))} style={{ background: 'var(--input-bg, var(--surface2))', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 8px', color: 'var(--text)', fontSize: 13 }}>{DESTINATIONS.map(o => <option key={o.value} value={o.value}>{_(o.label)}</option>)}</select></td>
              <td style={{ padding: '6px 8px' }}><select value={editForm.action} onChange={e => setEditForm(f => ({ ...f, action: e.target.value }))} style={{ background: 'var(--input-bg, var(--surface2))', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 8px', color: 'var(--text)', fontSize: 13 }}>{ACTIONS.map(o => <option key={o.value} value={o.value}>{_(o.label)}</option>)}</select></td>
              <td style={{ padding: '6px 8px' }}><input type="number" value={editForm.priority} onChange={e => setEditForm(f => ({ ...f, priority: e.target.value }))} style={{ width: 60, background: 'var(--input-bg, var(--surface2))', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 8px', color: 'var(--text)', fontSize: 13 }} /></td>
              <td style={{ padding: '6px 8px', textAlign: 'center' }}><input type="checkbox" checked={!!editForm.enabled} onChange={e => setEditForm(f => ({ ...f, enabled: e.target.checked }))} /></td>
              <td style={{ padding: '6px 8px', textAlign: 'right', whiteSpace: 'nowrap' }}>
                <button onClick={() => update(row.id)} style={{ marginRight: 4, background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 5, padding: '4px 10px', cursor: 'pointer', fontSize: 12 }}><Check size={12} /></button>
                <button onClick={() => setEditing(null)} style={{ background: 'transparent', color: 'var(--subtext)', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 10px', cursor: 'pointer', fontSize: 12 }}><X size={12} /></button>
              </td>
            </tr>
          ) : (
            <tr key={row.id} style={{ borderBottom: '1px solid var(--border)' }}>
              <td style={{ padding: '8px 8px', fontFamily: 'monospace', fontSize: 12, maxWidth: 260, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{row.pattern}</td>
              <td style={{ padding: '8px 8px' }}><span style={{ background: row.destination === 'primary' ? 'var(--red-soft, rgba(239,68,68,.12))' : 'rgba(99,102,241,.12)', color: row.destination === 'primary' ? 'var(--red)' : '#818cf8', borderRadius: 4, padding: '2px 7px', fontSize: 11, fontWeight: 700 }}>{row.destination}</span></td>
              <td style={{ padding: '8px 8px', color: 'var(--subtext)', fontSize: 12 }}>{row.action}</td>
              <td style={{ padding: '8px 8px', textAlign: 'right', color: 'var(--subtext)', fontSize: 12 }}>{row.priority}</td>
              <td style={{ padding: '8px 8px', textAlign: 'center' }}>
                <span style={{ color: row.enabled ? 'var(--green)' : 'var(--subtext)', fontWeight: 700, fontSize: 12 }}>
                  {row.enabled ? '✓' : '—'}
                </span>
              </td>
              <td style={{ padding: '8px 8px', textAlign: 'right', whiteSpace: 'nowrap' }}>
                <button onClick={() => startEdit(row)} style={{ marginRight: 4, background: 'transparent', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 10px', cursor: 'pointer', color: 'var(--text)', fontSize: 12 }}><Edit2 size={12} /></button>
                <button onClick={() => remove(row.id)} style={{ background: 'transparent', border: '1px solid rgba(239,68,68,.4)', borderRadius: 5, padding: '4px 10px', cursor: 'pointer', color: 'var(--red)', fontSize: 12 }}><Trash2 size={12} /></button>
              </td>
            </tr>
          ))}
          {rows.length === 0 && !adding && (
            <tr><td colSpan={6} style={{ textAlign: 'center', padding: 24, color: 'var(--subtext)', fontSize: 13 }}>{_('No rules yet. Click "Add rule" to create the first one.')}</td></tr>
          )}
        </tbody>
      </table>
    </div>
  )
}

// ─── Rewrite Rules ────────────────────────────────────────────────────────────
function emptyRewrite() {
  return { name: '', pattern: '', replacement: '', priority: 100, enabled: true }
}

function RewriteRulesConfig() {
  const { _ } = useLingui()
  const [rows, setRows] = useState([])
  const [loading, setLoading] = useState(true)
  const [err, setErr] = useState(null)
  const [adding, setAdding] = useState(false)
  const [form, setForm] = useState(emptyRewrite())
  const [editing, setEditing] = useState(null)
  const [editForm, setEditForm] = useState(null)
  const [preview, setPreview] = useState('')
  const [previewInput, setPreviewInput] = useState('')

  const load = useCallback(async () => {
    try {
      setLoading(true)
      const d = await apiFetch('/api/config/rewrite-rules')
      setRows(d)
      setErr(null)
    } catch (e) { setErr(e.message) }
    finally { setLoading(false) }
  }, [])

  useEffect(() => { load() }, [load])

  // live regex preview
  useEffect(() => {
    if (!form.pattern || !previewInput) { setPreview(''); return }
    try {
      const re = new RegExp(form.pattern, 'i')
      const result = previewInput.replace(re, form.replacement)
      setPreview(result)
    } catch { setPreview('⚠ Invalid regex') }
  }, [form.pattern, form.replacement, previewInput])

  const save = async () => {
    try {
      await apiFetch('/api/config/rewrite-rules', {
        method: 'POST',
        body: JSON.stringify({ ...form, priority: Number(form.priority) }),
      })
      setAdding(false)
      setForm(emptyRewrite())
      setPreviewInput('')
      load()
    } catch (e) { setErr(e.message) }
  }

  const update = async (id) => {
    try {
      await apiFetch(`/api/config/rewrite-rules/${id}`, {
        method: 'PUT',
        body: JSON.stringify({ ...editForm, priority: Number(editForm.priority) }),
      })
      setEditing(null)
      load()
    } catch (e) { setErr(e.message) }
  }

  const remove = async (id) => {
    if (!window.confirm(_('Delete this rewrite rule?'))) return
    try {
      await apiFetch(`/api/config/rewrite-rules/${id}`, { method: 'DELETE' })
      load()
    } catch (e) { setErr(e.message) }
  }

  if (loading) return <div style={{ color: 'var(--subtext)', fontSize: 13 }}>{_('Loading…')}</div>

  return (
    <div>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 12 }}>
        <span style={{ fontSize: 13, color: 'var(--subtext)' }}>{rows.length} {rows.length !== 1 ? _('rewrite rules') : _('rewrite rule')}</span>
        <button onClick={() => setAdding(true)} style={{ display: 'flex', alignItems: 'center', gap: 6, background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 8, padding: '7px 14px', cursor: 'pointer', fontSize: 13, fontWeight: 600 }}>
          <PlusCircle size={14} /> {_('Add rule')}
        </button>
      </div>
      <ErrMsg err={err} />

      {adding && (
        <div style={{ background: 'var(--surface)', border: '1px solid var(--accent)', borderRadius: 10, padding: 16, marginBottom: 14 }}>
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10, marginBottom: 10 }}>
            <Field label={_('Name')} value={form.name} onChange={v => setForm(f => ({ ...f, name: v }))} placeholder="add-force-index" />
            <Field label={_('Priority')} value={form.priority} onChange={v => setForm(f => ({ ...f, priority: v }))} type="number" />
          </div>
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10, marginBottom: 10 }}>
            <Field label={_('Pattern (regex)')} value={form.pattern} onChange={v => setForm(f => ({ ...f, pattern: v }))} placeholder="^SELECT (.+) FROM orders" />
            <Field label={_('Replacement')} value={form.replacement} onChange={v => setForm(f => ({ ...f, replacement: v }))} placeholder="SELECT $1 FROM orders FORCE INDEX (idx_created)" />
          </div>
          {/* live preview */}
          <div style={{ marginBottom: 10 }}>
            <Field label={_('Test SQL (preview)')} value={previewInput} onChange={setPreviewInput} placeholder="SELECT * FROM orders WHERE user_id=1" />
            {preview && (
              <div style={{ marginTop: 6, padding: '6px 10px', background: 'rgba(99,102,241,.1)', borderRadius: 6, fontFamily: 'monospace', fontSize: 12, color: preview.startsWith('⚠') ? 'var(--red)' : 'var(--text)' }}>
                {preview}
              </div>
            )}
          </div>
          <Toggle label={_('Enabled')} value={form.enabled} onChange={v => setForm(f => ({ ...f, enabled: v }))} />
          <div style={{ display: 'flex', gap: 8, marginTop: 12 }}>
            <button onClick={save} style={{ background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 7, padding: '6px 16px', cursor: 'pointer', fontWeight: 600, fontSize: 13 }}>{_('Save')}</button>
            <button onClick={() => { setAdding(false); setErr(null) }} style={{ background: 'transparent', color: 'var(--subtext)', border: '1px solid var(--border)', borderRadius: 7, padding: '6px 14px', cursor: 'pointer', fontSize: 13 }}>{_('Cancel')}</button>
          </div>
        </div>
      )}

      <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
        <thead>
          <tr style={{ borderBottom: '1px solid var(--border)', color: 'var(--subtext)', fontSize: 11, textTransform: 'uppercase' }}>
            <th style={{ textAlign: 'left', padding: '6px 8px' }}>{_('Name')}</th>
            <th style={{ textAlign: 'left', padding: '6px 8px' }}>{_('Pattern')}</th>
            <th style={{ textAlign: 'left', padding: '6px 8px' }}>{_('Replacement')}</th>
            <th style={{ textAlign: 'right', padding: '6px 8px' }}>{_('Priority')}</th>
            <th style={{ textAlign: 'center', padding: '6px 8px' }}>{_('On')}</th>
            <th style={{ textAlign: 'right', padding: '6px 8px' }}></th>
          </tr>
        </thead>
        <tbody>
          {rows.map(row => editing === row.id ? (
            <tr key={row.id} style={{ background: 'var(--surface2)' }}>
              <td style={{ padding: '6px 8px' }}><input value={editForm.name} onChange={e => setEditForm(f => ({ ...f, name: e.target.value }))} style={{ width: '100%', background: 'var(--input-bg, var(--surface2))', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 8px', color: 'var(--text)', fontSize: 13 }} /></td>
              <td style={{ padding: '6px 8px' }}><input value={editForm.pattern} onChange={e => setEditForm(f => ({ ...f, pattern: e.target.value }))} style={{ width: '100%', background: 'var(--input-bg, var(--surface2))', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 8px', color: 'var(--text)', fontSize: 12, fontFamily: 'monospace' }} /></td>
              <td style={{ padding: '6px 8px' }}><input value={editForm.replacement} onChange={e => setEditForm(f => ({ ...f, replacement: e.target.value }))} style={{ width: '100%', background: 'var(--input-bg, var(--surface2))', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 8px', color: 'var(--text)', fontSize: 12, fontFamily: 'monospace' }} /></td>
              <td style={{ padding: '6px 8px' }}><input type="number" value={editForm.priority} onChange={e => setEditForm(f => ({ ...f, priority: e.target.value }))} style={{ width: 60, background: 'var(--input-bg, var(--surface2))', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 8px', color: 'var(--text)', fontSize: 13 }} /></td>
              <td style={{ padding: '6px 8px', textAlign: 'center' }}><input type="checkbox" checked={!!editForm.enabled} onChange={e => setEditForm(f => ({ ...f, enabled: e.target.checked }))} /></td>
              <td style={{ padding: '6px 8px', textAlign: 'right', whiteSpace: 'nowrap' }}>
                <button onClick={() => update(row.id)} style={{ marginRight: 4, background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 5, padding: '4px 10px', cursor: 'pointer', fontSize: 12 }}><Check size={12} /></button>
                <button onClick={() => setEditing(null)} style={{ background: 'transparent', color: 'var(--subtext)', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 10px', cursor: 'pointer', fontSize: 12 }}><X size={12} /></button>
              </td>
            </tr>
          ) : (
            <tr key={row.id} style={{ borderBottom: '1px solid var(--border)' }}>
              <td style={{ padding: '8px 8px', fontWeight: 600, fontSize: 13 }}>{row.name}</td>
              <td style={{ padding: '8px 8px', fontFamily: 'monospace', fontSize: 11, maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', color: 'var(--subtext)' }}>{row.pattern}</td>
              <td style={{ padding: '8px 8px', fontFamily: 'monospace', fontSize: 11, maxWidth: 200, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', color: 'var(--subtext)' }}>{row.replacement}</td>
              <td style={{ padding: '8px 8px', textAlign: 'right', color: 'var(--subtext)', fontSize: 12 }}>{row.priority}</td>
              <td style={{ padding: '8px 8px', textAlign: 'center', color: row.enabled ? 'var(--green)' : 'var(--subtext)', fontWeight: 700, fontSize: 12 }}>{row.enabled ? '✓' : '—'}</td>
              <td style={{ padding: '8px 8px', textAlign: 'right', whiteSpace: 'nowrap' }}>
                <button onClick={() => { setEditing(row.id); setEditForm({ ...row }) }} style={{ marginRight: 4, background: 'transparent', border: '1px solid var(--border)', borderRadius: 5, padding: '4px 10px', cursor: 'pointer', color: 'var(--text)', fontSize: 12 }}><Edit2 size={12} /></button>
                <button onClick={() => remove(row.id)} style={{ background: 'transparent', border: '1px solid rgba(239,68,68,.4)', borderRadius: 5, padding: '4px 10px', cursor: 'pointer', color: 'var(--red)', fontSize: 12 }}><Trash2 size={12} /></button>
              </td>
            </tr>
          ))}
          {rows.length === 0 && !adding && (
            <tr><td colSpan={6} style={{ textAlign: 'center', padding: 24, color: 'var(--subtext)', fontSize: 13 }}>{_('No rewrite rules yet.')}</td></tr>
          )}
        </tbody>
      </table>
    </div>
  )
}

// ─── Backends ─────────────────────────────────────────────────────────────────
function emptyBackend() {
  return { host: '', port: 3306, role: 'replica', weight: 100, user: '', password: '', database: '' }
}

const ROLES = [
  { value: 'primary', label: 'Primary' },
  { value: 'replica', label: 'Replica' },
]

function BackendsConfig({ capabilities }) {
  const { _ } = useLingui()
  const canMysql = capabilities?.mysql_proxy_enabled !== false
  const canPg = capabilities?.pgsql_proxy_enabled === true
  const availableProtocols = [
    ...(canMysql ? ['mysql'] : []),
    ...(canPg ? ['pgsql'] : []),
  ]

  const [protocol, setProtocol] = useState(canMysql ? 'mysql' : (canPg ? 'pgsql' : 'mysql'))
  const [rows, setRows] = useState([])
  const [loading, setLoading] = useState(true)
  const [err, setErr] = useState(null)
  const [adding, setAdding] = useState(false)
  const [form, setForm] = useState(emptyBackend())

  const endpointBase = '/api/config/backends'
  const endpoint = `${endpointBase}?protocol=${protocol}`
  const isPg = protocol === 'pgsql'

  useEffect(() => {
    if (!availableProtocols.includes(protocol)) {
      setProtocol(availableProtocols[0] ?? 'mysql')
    }
  }, [availableProtocols, protocol])

  const load = useCallback(async () => {
    if (availableProtocols.length === 0) {
      setRows([])
      setLoading(false)
      return
    }
    try {
      setLoading(true)
      const d = await apiFetch(endpoint)
      setRows(d)
      setErr(null)
    } catch (e) { setErr(e.message) }
    finally { setLoading(false) }
  }, [endpoint, availableProtocols.length])

  useEffect(() => { load() }, [load])

  useEffect(() => {
    setAdding(false)
    setForm(emptyBackend())
  }, [protocol])

  const save = async () => {
    try {
      await apiFetch(endpoint, {
        method: 'POST',
        body: JSON.stringify({ ...form, port: Number(form.port), weight: Number(form.weight) }),
      })
      setAdding(false)
      setForm(emptyBackend())
      load()
    } catch (e) { setErr(e.message) }
  }

  const remove = async (id) => {
    if (!window.confirm(_('Remove this backend? Active connections will be drained.'))) return
    try {
      await apiFetch(`${endpointBase}/${id}?protocol=${protocol}`, { method: 'DELETE' })
      load()
    } catch (e) { setErr(e.message) }
  }

  if (loading) return <div style={{ color: 'var(--subtext)', fontSize: 13 }}>{_('Loading…')}</div>

  if (availableProtocols.length === 0) {
    return (
      <div style={{
        border: '1px solid var(--border)',
        borderRadius: 10,
        padding: 14,
        background: 'var(--surface)',
        color: 'var(--subtext)',
        fontSize: 13,
      }}>
        {_('No database proxy is enabled in this environment.')}
      </div>
    )
  }

  return (
    <div>
      <div style={{
        marginBottom: 14,
        border: '1px solid var(--border)',
        borderRadius: 10,
        padding: 12,
        background: 'var(--surface)',
      }}>
        <div style={{ fontSize: 12, fontWeight: 700, marginBottom: 4, letterSpacing: '.03em', textTransform: 'uppercase', color: 'var(--subtext)' }}>
          {_('Scope')}
        </div>
        <div style={{ fontSize: 13, lineHeight: 1.5 }}>
          {_('This tab manages runtime backends for the selected protocol. Changes are applied immediately to the in-memory pool.')}
        </div>
        {availableProtocols.length > 1 && (
          <div style={{ display: 'flex', gap: 8, marginTop: 10, flexWrap: 'wrap' }}>
            {canMysql && (
              <button
                onClick={() => setProtocol('mysql')}
                style={{
                  background: protocol === 'mysql' ? 'var(--accent)' : 'transparent',
                  color: protocol === 'mysql' ? '#fff' : 'var(--text)',
                  border: '1px solid var(--border)',
                  borderRadius: 999,
                  padding: '4px 12px',
                  cursor: 'pointer',
                  fontSize: 12,
                  fontWeight: 700,
                }}
              >
                MySQL
              </button>
            )}
            {canPg && (
              <button
                onClick={() => setProtocol('pgsql')}
                style={{
                  background: protocol === 'pgsql' ? 'var(--accent)' : 'transparent',
                  color: protocol === 'pgsql' ? '#fff' : 'var(--text)',
                  border: '1px solid var(--border)',
                  borderRadius: 999,
                  padding: '4px 12px',
                  cursor: 'pointer',
                  fontSize: 12,
                  fontWeight: 700,
                }}
              >
                PostgreSQL
              </button>
            )}
          </div>
        )}
      </div>

      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 12 }}>
        <span style={{ fontSize: 13, color: 'var(--subtext)' }}>
          {protocol === 'mysql' ? 'MySQL' : 'PostgreSQL'}: {rows.length} {rows.length !== 1 ? _('backends') : _('backend')}
        </span>
        <button onClick={() => setAdding(true)} style={{ display: 'flex', alignItems: 'center', gap: 6, background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 8, padding: '7px 14px', cursor: 'pointer', fontSize: 13, fontWeight: 600 }}>
          <PlusCircle size={14} /> {_('Add backend')}
        </button>
      </div>
      <ErrMsg err={err} />

      {adding && (
        <div style={{ background: 'var(--surface)', border: '1px solid var(--accent)', borderRadius: 10, padding: 16, marginBottom: 14 }}>
          <div style={{ display: 'grid', gridTemplateColumns: '2fr 80px 1fr 80px', gap: 10, marginBottom: 10 }}>
            <Field label={_('Host')} value={form.host} onChange={v => setForm(f => ({ ...f, host: v }))} placeholder="db-replica-1.internal" />
            <Field label={_('Port')} value={form.port} onChange={v => setForm(f => ({ ...f, port: v }))} type="number" />
            <Select label={_('Role')} value={form.role} onChange={v => setForm(f => ({ ...f, role: v }))} options={ROLES} />
            <Field label={_('Weight')} value={form.weight} onChange={v => setForm(f => ({ ...f, weight: v }))} type="number" />
          </div>
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr 1fr', gap: 10, marginBottom: 12 }}>
            <Field label={_('User')} value={form.user} onChange={v => setForm(f => ({ ...f, user: v }))} placeholder={isPg ? 'postgres' : 'proxy_user'} />
            <Field label={_('Password')} value={form.password} onChange={v => setForm(f => ({ ...f, password: v }))} type="password" />
            <Field label={_('Database')} value={form.database} onChange={v => setForm(f => ({ ...f, database: v }))} placeholder={isPg ? 'postgres' : 'myapp'} />
          </div>
          <div style={{ display: 'flex', gap: 8 }}>
            <button onClick={save} style={{ background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 7, padding: '6px 16px', cursor: 'pointer', fontWeight: 600, fontSize: 13 }}>{_('Save')}</button>
            <button onClick={() => { setAdding(false); setErr(null) }} style={{ background: 'transparent', color: 'var(--subtext)', border: '1px solid var(--border)', borderRadius: 7, padding: '6px 14px', cursor: 'pointer', fontSize: 13 }}>{_('Cancel')}</button>
          </div>
        </div>
      )}

      <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
        <thead>
          <tr style={{ borderBottom: '1px solid var(--border)', color: 'var(--subtext)', fontSize: 11, textTransform: 'uppercase' }}>
            <th style={{ textAlign: 'left', padding: '6px 8px' }}>{_('Host')}</th>
            <th style={{ textAlign: 'right', padding: '6px 8px' }}>{_('Port')}</th>
            <th style={{ textAlign: 'left', padding: '6px 8px' }}>{_('Role')}</th>
            <th style={{ textAlign: 'right', padding: '6px 8px' }}>{_('Weight')}</th>
            <th style={{ textAlign: 'left', padding: '6px 8px' }}>{_('User')}</th>
            <th style={{ textAlign: 'right', padding: '6px 8px' }}></th>
          </tr>
        </thead>
        <tbody>
          {rows.map(row => (
            <tr key={row.id} style={{ borderBottom: '1px solid var(--border)' }}>
              <td style={{ padding: '8px 8px', fontFamily: 'monospace', fontSize: 12 }}>{row.addr?.split(':').slice(0, -1).join(':') || row.host || '—'}</td>
              <td style={{ padding: '8px 8px', textAlign: 'right', color: 'var(--subtext)', fontSize: 12 }}>{row.addr?.split(':').at(-1) || row.port || '—'}</td>
              <td style={{ padding: '8px 8px' }}>
                <span style={{ background: row.role === 'primary' ? 'var(--red-soft, rgba(239,68,68,.12))' : 'rgba(99,102,241,.12)', color: row.role === 'primary' ? 'var(--red)' : '#818cf8', borderRadius: 4, padding: '2px 7px', fontSize: 11, fontWeight: 700 }}>
                  {row.role}
                </span>
              </td>
              <td style={{ padding: '8px 8px', textAlign: 'right', color: 'var(--subtext)', fontSize: 12 }}>{row.weight}</td>
              <td style={{ padding: '8px 8px', color: 'var(--subtext)', fontSize: 12 }}>{row.user || '—'}</td>
              <td style={{ padding: '8px 8px', textAlign: 'right' }}>
                <button onClick={() => remove(row.id)} style={{ background: 'transparent', border: '1px solid rgba(239,68,68,.4)', borderRadius: 5, padding: '4px 10px', cursor: 'pointer', color: 'var(--red)', fontSize: 12 }}><Trash2 size={12} /></button>
              </td>
            </tr>
          ))}
          {rows.length === 0 && !adding && (
            <tr><td colSpan={6} style={{ textAlign: 'center', padding: 24, color: 'var(--subtext)', fontSize: 13 }}>{_('No backends configured for this protocol.')}</td></tr>
          )}
        </tbody>
      </table>
    </div>
  )
}

// ─── Users ────────────────────────────────────────────────────────────────────
function emptyUser() {
  return { username: '', password: '', allow_writes: false, max_connections: 50 }
}

function UsersConfig() {
  const { _ } = useLingui()
  const [rows, setRows] = useState([])
  const [loading, setLoading] = useState(true)
  const [err, setErr] = useState(null)
  const [adding, setAdding] = useState(false)
  const [form, setForm] = useState(emptyUser())

  const load = useCallback(async () => {
    try {
      setLoading(true)
      const d = await apiFetch('/api/config/users')
      setRows(d)
      setErr(null)
    } catch (e) { setErr(e.message) }
    finally { setLoading(false) }
  }, [])

  useEffect(() => { load() }, [load])

  const save = async () => {
    try {
      await apiFetch('/api/config/users', {
        method: 'POST',
        body: JSON.stringify({ ...form, max_connections: Number(form.max_connections) }),
      })
      setAdding(false)
      setForm(emptyUser())
      load()
    } catch (e) { setErr(e.message) }
  }

  const remove = async (id) => {
    if (!window.confirm(_('Delete this user? Existing sessions will be terminated on next auth.'))) return
    try {
      await apiFetch(`/api/config/users/${id}`, { method: 'DELETE' })
      load()
    } catch (e) { setErr(e.message) }
  }

  if (loading) return <div style={{ color: 'var(--subtext)', fontSize: 13 }}>{_('Loading…')}</div>

  return (
    <div>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 12 }}>
        <span style={{ fontSize: 13, color: 'var(--subtext)' }}>{rows.length} {rows.length !== 1 ? _('users') : _('user')}</span>
        <button onClick={() => setAdding(true)} style={{ display: 'flex', alignItems: 'center', gap: 6, background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 8, padding: '7px 14px', cursor: 'pointer', fontSize: 13, fontWeight: 600 }}>
          <PlusCircle size={14} /> {_('Add user')}
        </button>
      </div>
      <ErrMsg err={err} />

      {adding && (
        <div style={{ background: 'var(--surface)', border: '1px solid var(--accent)', borderRadius: 10, padding: 16, marginBottom: 14 }}>
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr 80px', gap: 10, marginBottom: 10 }}>
            <Field label={_('Username')} value={form.username} onChange={v => setForm(f => ({ ...f, username: v }))} placeholder="app_user" />
            <Field label={_('Password')} value={form.password} onChange={v => setForm(f => ({ ...f, password: v }))} type="password" />
            <Field label={_('Max Conn')} value={form.max_connections} onChange={v => setForm(f => ({ ...f, max_connections: v }))} type="number" />
          </div>
          <Toggle label={_('Allow writes')} value={form.allow_writes} onChange={v => setForm(f => ({ ...f, allow_writes: v }))} />
          <div style={{ display: 'flex', gap: 8, marginTop: 12 }}>
            <button onClick={save} style={{ background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 7, padding: '6px 16px', cursor: 'pointer', fontWeight: 600, fontSize: 13 }}>{_('Save')}</button>
            <button onClick={() => { setAdding(false); setErr(null) }} style={{ background: 'transparent', color: 'var(--subtext)', border: '1px solid var(--border)', borderRadius: 7, padding: '6px 14px', cursor: 'pointer', fontSize: 13 }}>{_('Cancel')}</button>
          </div>
        </div>
      )}

      <table style={{ width: '100%', borderCollapse: 'collapse', fontSize: 13 }}>
        <thead>
          <tr style={{ borderBottom: '1px solid var(--border)', color: 'var(--subtext)', fontSize: 11, textTransform: 'uppercase' }}>
            <th style={{ textAlign: 'left', padding: '6px 8px' }}>{_('Username')}</th>
            <th style={{ textAlign: 'center', padding: '6px 8px' }}>{_('Allow Writes')}</th>
            <th style={{ textAlign: 'right', padding: '6px 8px' }}>{_('Max Conn')}</th>
            <th style={{ textAlign: 'right', padding: '6px 8px' }}></th>
          </tr>
        </thead>
        <tbody>
          {rows.map(row => (
            <tr key={row.id} style={{ borderBottom: '1px solid var(--border)' }}>
              <td style={{ padding: '8px 8px', fontWeight: 600 }}>{row.username}</td>
              <td style={{ padding: '8px 8px', textAlign: 'center' }}>
                <span style={{ color: row.allow_writes ? 'var(--green)' : 'var(--subtext)', fontWeight: 700, fontSize: 12 }}>
                  {row.allow_writes ? _('Yes') : _('No')}
                </span>
              </td>
              <td style={{ padding: '8px 8px', textAlign: 'right', color: 'var(--subtext)', fontSize: 12 }}>{row.max_connections ?? '—'}</td>
              <td style={{ padding: '8px 8px', textAlign: 'right' }}>
                <button onClick={() => remove(row.id)} style={{ background: 'transparent', border: '1px solid rgba(239,68,68,.4)', borderRadius: 5, padding: '4px 10px', cursor: 'pointer', color: 'var(--red)', fontSize: 12 }}><Trash2 size={12} /></button>
              </td>
            </tr>
          ))}
          {rows.length === 0 && !adding && (
            <tr><td colSpan={4} style={{ textAlign: 'center', padding: 24, color: 'var(--subtext)', fontSize: 13 }}>{_('No users configured.')}</td></tr>
          )}
        </tbody>
      </table>
    </div>
  )
}

// ─── Config History ───────────────────────────────────────────────────────────
function ConfigHistory() {
  const { _ } = useLingui()
  const [rows, setRows] = useState([])
  const [loading, setLoading] = useState(true)
  const [err, setErr] = useState(null)
  const [limit, setLimit] = useState(50)

  const load = useCallback(async () => {
    try {
      setLoading(true)
      const d = await apiFetch(`/api/config/history?limit=${limit}`)
      setRows(d)
      setErr(null)
    } catch (e) { setErr(e.message) }
    finally { setLoading(false) }
  }, [limit])

  useEffect(() => { load() }, [load])

  const exportConfig = async (setErrFn) => {
    try {
      const res = await fetch('/api/config/export', { headers: authHeaders() })
      if (!res.ok) throw new Error(await res.text())
      const text = await res.text()
      const blob = new Blob([text], { type: 'text/plain' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url; a.download = 'turbineproxy.toml'
      a.click(); URL.revokeObjectURL(url)
    } catch (e) { if (setErrFn) setErrFn(e.message) }
  }

  if (loading) return <div style={{ color: 'var(--subtext)', fontSize: 13 }}>{_('Loading…')}</div>

  return (
    <div>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 12 }}>
        <span style={{ fontSize: 13, color: 'var(--subtext)' }}>{rows.length} {rows.length !== 1 ? _('changes') : _('change')}</span>
        <select value={limit} onChange={e => setLimit(Number(e.target.value))} style={{ background: 'var(--surface2)', border: '1px solid var(--border)', borderRadius: 6, padding: '5px 10px', color: 'var(--text)', fontSize: 13 }}>
          {[20, 50, 100, 200].map(n => <option key={n} value={n}>{_('Last')} {n}</option>)}
        </select>
      </div>
      <ErrMsg err={err} />

      {rows.length === 0 ? (
        <div style={{ textAlign: 'center', padding: 40, color: 'var(--subtext)', fontSize: 13 }}>
          {_('No config changes recorded yet.')}
        </div>
      ) : (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
          {rows.map((row, i) => (
            <div key={i} style={{ background: 'var(--surface)', borderRadius: 10, border: '1px solid var(--border)', padding: '12px 16px' }}>
              <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 6 }}>
                <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
                  <span style={{
                    fontSize: 11, fontWeight: 700, padding: '2px 8px', borderRadius: 4,
                    background: row.action === 'create' ? 'rgba(34,197,94,.15)' : row.action === 'delete' ? 'rgba(239,68,68,.15)' : 'rgba(99,102,241,.15)',
                    color: row.action === 'create' ? 'var(--green)' : row.action === 'delete' ? 'var(--red)' : '#818cf8',
                    textTransform: 'uppercase',
                  }}>{row.action}</span>
                  <span style={{ fontSize: 12, color: 'var(--subtext)' }}>{row.entity_type}</span>
                  {row.entity_id && <span style={{ fontSize: 11, color: 'var(--subtext)', fontFamily: 'monospace' }}>#{row.entity_id}</span>}
                </div>
                <div style={{ display: 'flex', gap: 12, alignItems: 'center' }}>
                  {row.changed_by && <span style={{ fontSize: 11, color: 'var(--subtext)' }}>{row.changed_by}</span>}
                  <span style={{ fontSize: 11, color: 'var(--subtext)', display: 'flex', alignItems: 'center', gap: 4 }}>
                    <Clock size={10} /> {new Date(row.changed_at * 1000).toLocaleString()}
                  </span>
                </div>
              </div>
              {(row.before_json || row.after_json) && (
                <div style={{ display: 'grid', gridTemplateColumns: row.before_json && row.after_json ? '1fr 1fr' : '1fr', gap: 8, marginTop: 6 }}>
                  {row.before_json && (
                    <div>
                      <div style={{ fontSize: 10, color: 'var(--subtext)', textTransform: 'uppercase', marginBottom: 3 }}>{_('Before')}</div>
                      <pre style={{ margin: 0, fontSize: 11, color: 'var(--subtext)', background: 'var(--bg)', borderRadius: 6, padding: '6px 10px', overflow: 'auto', maxHeight: 100 }}>{JSON.stringify(JSON.parse(row.before_json), null, 2)}</pre>
                    </div>
                  )}
                  {row.after_json && (
                    <div>
                      <div style={{ fontSize: 10, color: 'var(--subtext)', textTransform: 'uppercase', marginBottom: 3 }}>{_('After')}</div>
                      <pre style={{ margin: 0, fontSize: 11, color: 'var(--text)', background: 'var(--bg)', borderRadius: 6, padding: '6px 10px', overflow: 'auto', maxHeight: 100 }}>{JSON.stringify(JSON.parse(row.after_json), null, 2)}</pre>
                    </div>
                  )}
                </div>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

// ─── ConfigPanel (root) ───────────────────────────────────────────────────────

// ─── Import modal ─────────────────────────────────────────────────────────────
function ImportModal({ onClose, onDone }) {
  const { _ } = useLingui()
  const [toml, setToml] = useState('')
  const [err, setErr] = useState(null)
  const [loading, setLoading] = useState(false)
  const fileRef = useRef()

  const onFile = (e) => {
    const f = e.target.files?.[0]
    if (!f) return
    const reader = new FileReader()
    reader.onload = (ev) => setToml(ev.target.result)
    reader.readAsText(f)
  }

  const apply = async () => {
    if (!toml.trim()) { setErr(_('Paste or upload a TOML file first.')); return }
    try {
      setLoading(true)
      const res = await fetch('/api/config/import', {
        method: 'POST',
        headers: { ...authHeaders(), 'Content-Type': 'text/plain' },
        body: toml,
      })
      if (!res.ok) throw new Error(await res.text())
      onDone()
    } catch (e) { setErr(e.message) }
    finally { setLoading(false) }
  }

  return (
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,.55)',
      display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 1000,
    }} onClick={e => e.target === e.currentTarget && onClose()}>
      <div style={{
        background: 'var(--surface, #ffffff)', border: '1px solid var(--border)', borderRadius: 14,
        padding: 24, width: 640, maxWidth: '95vw', maxHeight: '85vh',
        display: 'flex', flexDirection: 'column', gap: 14,
        boxShadow: '0 8px 40px rgba(0,0,0,.55)',
      }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
          <span style={{ fontWeight: 700, fontSize: 15 }}>{_('Import TOML config')}</span>
          <button onClick={onClose} style={{ background: 'transparent', border: 'none', cursor: 'pointer', color: 'var(--subtext)', padding: 4 }}><X size={16} /></button>
        </div>
        <div style={{ fontSize: 12, color: 'var(--subtext)', lineHeight: 1.5 }}>
          Upload or paste a <code style={{ background: 'var(--surface2)', padding: '1px 5px', borderRadius: 4 }}>turbineproxy.toml</code> file.
          Rules, rewrite rules, backends and users will be <strong>replaced</strong>. Infrastructure settings (<code style={{ background: 'var(--surface2)', padding: '1px 5px', borderRadius: 4 }}>[proxy]</code>, <code style={{ background: 'var(--surface2)', padding: '1px 5px', borderRadius: 4 }}>[tls]</code>) are ignored.
        </div>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
          <button
            onClick={() => fileRef.current?.click()}
            style={{ display: 'flex', alignItems: 'center', gap: 6, background: 'var(--surface2)', border: '1px solid var(--border)', borderRadius: 7, padding: '6px 14px', cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}
          >
            <Upload size={13} /> {_('Choose file')}
          </button>
          <input ref={fileRef} type="file" accept=".toml,text/plain" onChange={onFile} style={{ display: 'none' }} />
          {toml && <span style={{ fontSize: 12, color: 'var(--green)' }}>✓ {toml.split('\n').length} {_('lines loaded')}</span>}
        </div>
        <textarea
          value={toml}
          onChange={e => setToml(e.target.value)}
          placeholder="# Paste turbineproxy.toml here…"
          style={{
            flex: 1, minHeight: 260, background: 'var(--surface2)', border: '1px solid var(--border)',
            borderRadius: 8, padding: '10px 12px', color: 'var(--text)', fontFamily: 'monospace',
            fontSize: 12, resize: 'vertical', lineHeight: 1.6,
          }}
        />
        <ErrMsg err={err} />
        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
          <button onClick={onClose} style={{ background: 'transparent', color: 'var(--subtext)', border: '1px solid var(--border)', borderRadius: 7, padding: '7px 18px', cursor: 'pointer', fontSize: 13 }}>{_('Cancel')}</button>
          <button
            onClick={apply}
            disabled={loading || !toml.trim()}
            style={{ background: 'var(--accent)', color: '#fff', border: 'none', borderRadius: 7, padding: '7px 20px', cursor: loading ? 'wait' : 'pointer', fontWeight: 700, fontSize: 13, opacity: (!toml.trim() || loading) ? .5 : 1 }}
          >
            {loading ? _('Applying…') : _('Apply & reload')}
          </button>
        </div>
      </div>
    </div>
  )
}

export function ConfigPanel({ capabilities }) {
  const { _ } = useLingui()
  const [sub, setSub] = useState('rules')
  const [exportErr, setExportErr] = useState(null)
  const [showImport, setShowImport] = useState(false)

  const CONFIG_TABS = [
    { key: 'rules',    label: _('Query Rules'),   Icon: Filter },
    { key: 'rewrite',  label: _('Rewrite Rules'), Icon: Pencil },
    { key: 'backends', label: _('Backends'),      Icon: Server },
    { key: 'users',    label: _('Users'),         Icon: UsersIcon },
    { key: 'history',  label: _('History'),       Icon: Clock },
  ]

  const tabs = CONFIG_TABS.filter(t => {
    if (t.key === 'backends' && capabilities?.mysql_runtime_backends_supported === false) {
      return false
    }
    return true
  })

  useEffect(() => {
    if (!tabs.find(t => t.key === sub)) {
      setSub(tabs[0]?.key ?? 'rules')
    }
  }, [sub, tabs])

  const doExport = async () => {
    try {
      setExportErr(null)
      const res = await fetch('/api/config/export', { headers: authHeaders() })
      if (!res.ok) throw new Error(await res.text())
      const text = await res.text()
      const blob = new Blob([text], { type: 'text/plain' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url; a.download = 'turbineproxy.toml'
      a.click(); URL.revokeObjectURL(url)
    } catch (e) { setExportErr(e.message) }
  }

  return (
    <div>
      {/* ── Action bar ── */}
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 18 }}>
        <SubNav tabs={tabs} active={sub} setActive={setSub} />
        <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexShrink: 0 }}>
          {exportErr && <span style={{ fontSize: 12, color: 'var(--red)' }}>{exportErr}</span>}
          <button
            onClick={doExport}
            style={{ display: 'flex', alignItems: 'center', gap: 6, background: 'var(--surface2)', color: 'var(--text)', border: '1px solid var(--border)', borderRadius: 8, padding: '7px 14px', cursor: 'pointer', fontSize: 13, whiteSpace: 'nowrap' }}
          >
            <Download size={13} /> {_('Export TOML')}
          </button>
          <button
            onClick={() => setShowImport(true)}
            style={{ display: 'flex', alignItems: 'center', gap: 6, background: 'var(--surface2)', color: 'var(--text)', border: '1px solid var(--border)', borderRadius: 8, padding: '7px 14px', cursor: 'pointer', fontSize: 13, whiteSpace: 'nowrap' }}
          >
            <Upload size={13} /> {_('Import TOML')}
          </button>
        </div>
      </div>

      {sub === 'rules'    && <QueryRulesConfig />}
      {sub === 'rewrite'  && <RewriteRulesConfig />}
      {sub === 'backends' && <BackendsConfig capabilities={capabilities} />}
      {sub === 'users'    && <UsersConfig />}
      {sub === 'history'  && <ConfigHistory />}

      {capabilities?.pgsql_proxy_enabled && capabilities?.pgsql_runtime_backends_supported === false && (
        <div style={{
          marginTop: 14,
          border: '1px solid var(--border)',
          borderRadius: 10,
          padding: 12,
          background: 'var(--surface)',
          fontSize: 13,
          color: 'var(--subtext)',
        }}>
          {_('PostgreSQL is enabled, but runtime backend CRUD is not available yet. Use the Infrastructure views for PostgreSQL topology and pool health.')}
        </div>
      )}

      {showImport && (
        <ImportModal
          onClose={() => setShowImport(false)}
          onDone={() => { setShowImport(false); setSub('history') }}
        />
      )}
    </div>
  )
}
