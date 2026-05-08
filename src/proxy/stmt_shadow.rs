//! Statement shadowing — transparent prepared-statement tracking for connection-pool proxies.
//!
//! # PostgreSQL statement shadowing
//! PG prepared statements are named strings scoped to one backend session.
//! `PgStmtShadow` tracks every named statement issued by a client.  When
//! `stmt_conn` dies, the proxy re-issues `PREPARE` for all tracked statements
//! on a fresh backend connection before replaying the failed pipeline.
//! When all statements are closed the sticky `stmt_conn` is returned to the
//! pool immediately — fixing the resource leak in the original implementation.
//!
//! # MySQL statement shadowing
//! MySQL prepared statements use a 4-byte integer `stmt_id` assigned by the
//! backend.  `MysqlStmtShadow` assigns a stable *proxy-level* `stmt_id` that
//! is remapped to the current backend's stmt_id on every `COM_STMT_*` command.
//! When `stmt_conn` dies during `COM_STMT_EXECUTE` the proxy silently
//! re-prepares all open statements on a new backend, updates the mapping, and
//! retries the execute.

use std::collections::HashMap;

// ══════════════════════════════════════════════════════════════════════════════
// ─── PostgreSQL ───────────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════════════

/// Names extracted from a PG extended-query pipeline.
#[derive(Debug, Default)]
pub struct PgPipelineScan {
    /// Named statements declared by `P` (Parse) messages in this pipeline.
    pub parses: Vec<PgParsedStmt>,
    /// Named statements closed by `C` (Close) messages with type byte `S`.
    pub closes: Vec<String>,
}

#[derive(Debug)]
pub struct PgParsedStmt {
    pub name:  String,
    pub query: String,
}

/// Scan a raw accumulated PG pipeline for Parse and Close messages.
///
/// `raw` is the concatenated bytes of `P/B/D/E/C/H/S` messages produced by
/// `PgClientSession::read_command` for `Command::Stmt`.
pub fn scan_pg_pipeline(raw: &[u8]) -> PgPipelineScan {
    let mut scan = PgPipelineScan::default();
    let mut pos  = 0;

    while pos + 5 <= raw.len() {
        let type_byte = raw[pos];
        let len = u32::from_be_bytes([raw[pos+1], raw[pos+2], raw[pos+3], raw[pos+4]]) as usize;
        if len < 4 { break; }
        let end = pos + 1 + len;
        if end > raw.len() { break; }
        let payload = &raw[pos + 5 .. end];
        pos = end;

        match type_byte {
            b'P' => {
                // Parse: stmt_name\0 + query\0 + num_type_oids(int16) + [oids...]
                if let Some((name, rest)) = read_cstr(payload) {
                    if let Some((query, _)) = read_cstr(rest) {
                        scan.parses.push(PgParsedStmt { name, query });
                    }
                }
            }
            b'C' => {
                // Close: type('S'=statement / 'P'=portal) + name\0
                if payload.len() >= 2 && payload[0] == b'S' {
                    if let Some((name, _)) = read_cstr(&payload[1..]) {
                        if !name.is_empty() {
                            scan.closes.push(name);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    scan
}

fn read_cstr(buf: &[u8]) -> Option<(String, &[u8])> {
    let end = buf.iter().position(|&b| b == 0)?;
    let s   = String::from_utf8_lossy(&buf[..end]).into_owned();
    Some((s, &buf[end + 1..]))
}

/// Build a mini-pipeline `Parse(name, query) + Sync` to re-prepare one statement.
///
/// The caller sends this via `BackendConnection::send_raw` and discards the
/// ParseComplete + ReadyForQuery response — it only checks for errors.
pub fn build_pg_reparse(name: &str, query: &str) -> Vec<u8> {
    // Parse payload: name\0 + query\0 + 0x0000 (zero explicit type OIDs)
    let mut payload = Vec::new();
    payload.extend_from_slice(name.as_bytes());  payload.push(0);
    payload.extend_from_slice(query.as_bytes()); payload.push(0);
    payload.extend_from_slice(&[0u8, 0u8]);      // num_type_oids = 0

    let parse_len = (payload.len() + 4) as u32;
    let mut msg   = Vec::with_capacity(5 + payload.len() + 5);
    msg.push(b'P');
    msg.extend_from_slice(&parse_len.to_be_bytes());
    msg.extend_from_slice(&payload);

    // Sync — tells backend to flush and send ReadyForQuery so we can confirm.
    msg.push(b'S');
    msg.extend_from_slice(&4u32.to_be_bytes()); // length = 4 (no body)
    msg
}

/// Per-session PostgreSQL statement shadow map.
///
/// Tracks every named prepared statement so the proxy can re-issue `PREPARE`
/// transparently on a fresh backend without the client noticing a drop.
#[derive(Default)]
pub struct PgStmtShadow {
    /// name → query text for every currently-open named prepared statement.
    stmts: HashMap<String, String>,
}

impl PgStmtShadow {
    pub fn new() -> Self { Self::default() }

    /// Update the shadow map from a pipeline scan.
    ///
    /// * Named `Parse` messages add entries.
    /// * `Close(S, name)` messages remove entries.
    pub fn apply_scan(&mut self, scan: &PgPipelineScan) {
        for p in &scan.parses {
            if !p.name.is_empty() {
                self.stmts.insert(p.name.clone(), p.query.clone());
            }
        }
        for name in &scan.closes {
            self.stmts.remove(name);
        }
    }

    /// Number of currently tracked named prepared statements.
    pub fn open_count(&self) -> usize { self.stmts.len() }

    /// Returns `true` when no named prepared statements are open.
    pub fn is_empty(&self) -> bool { self.stmts.is_empty() }

    /// Build re-prepare mini-pipelines for all tracked stmts, skipping any
    /// name in `skip` (those are being prepared by the current pipeline itself).
    pub fn build_reprepare_for(&self, skip: &std::collections::HashSet<String>) -> Vec<Vec<u8>> {
        self.stmts.iter()
            .filter(|(name, _)| !skip.contains(*name))
            .map(|(name, query)| build_pg_reparse(name, query))
            .collect()
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// ─── MySQL ────────────────────────────────────────────────────────────────────
// ══════════════════════════════════════════════════════════════════════════════

/// Metadata for a single MySQL proxy-level prepared statement.
#[derive(Clone, Debug)]
pub struct MysqlStmtInfo {
    /// Original query bytes (without the COM_STMT_PREPARE command byte 0x16).
    pub query: Vec<u8>,
    /// Number of parameters extracted from the PREPARE response.
    #[allow(dead_code)]
    pub num_params: u16,
    /// Number of result columns extracted from the PREPARE response.
    #[allow(dead_code)]
    pub num_columns: u16,
    /// The `stmt_id` the *current* backend assigned.  Updated on re-prepare.
    pub backend_id: u32,
}

/// Per-session MySQL statement shadow map with proxy-level stmt_id assignment.
///
/// The proxy assigns a stable, monotonically-increasing `proxy_id` per
/// `COM_STMT_PREPARE`.  All subsequent `COM_STMT_*` packets from the client
/// use `proxy_id`; the proxy rewrites them to `backend_id` before forwarding.
#[derive(Default)]
pub struct MysqlStmtShadow {
    stmts:   HashMap<u32, MysqlStmtInfo>,
    next_id: u32,
}

impl MysqlStmtShadow {
    pub fn new() -> Self { Self { stmts: HashMap::new(), next_id: 1 } }

    /// Register a new prepared statement; returns the `proxy_id` to send back to the client.
    pub fn register(
        &mut self,
        query:       Vec<u8>,
        num_params:  u16,
        num_columns: u16,
        backend_id:  u32,
    ) -> u32 {
        let proxy_id = self.next_id;
        self.next_id += 1;
        self.stmts.insert(proxy_id, MysqlStmtInfo { query, num_params, num_columns, backend_id });
        proxy_id
    }

    /// Translate proxy_id → backend_id (returns `None` when unknown).
    pub fn backend_id(&self, proxy_id: u32) -> Option<u32> {
        self.stmts.get(&proxy_id).map(|s| s.backend_id)
    }

    /// Get full info for a proxy_id.
    #[allow(dead_code)]
    pub fn get(&self, proxy_id: u32) -> Option<&MysqlStmtInfo> {
        self.stmts.get(&proxy_id)
    }

    /// Remove a statement (called on COM_STMT_CLOSE).
    pub fn remove(&mut self, proxy_id: u32) {
        self.stmts.remove(&proxy_id);
    }

    /// Number of currently open prepared statements.
    pub fn open_count(&self) -> u32 { self.stmts.len() as u32 }

    /// Returns `true` when no prepared statements are open.
    pub fn is_empty(&self) -> bool { self.stmts.is_empty() }

    /// Update backend_ids for all tracked statements after re-preparing on a new backend.
    pub fn update_backend_ids(&mut self, new_ids: &HashMap<u32, u32>) {
        for (proxy_id, backend_id) in new_ids {
            if let Some(info) = self.stmts.get_mut(proxy_id) {
                info.backend_id = *backend_id;
            }
        }
    }

    /// Collect (proxy_id, prepare_packet) pairs — owned data, safe across await.
    pub fn reprepare_jobs(&self) -> Vec<(u32, Vec<u8>)> {
        self.stmts.iter().map(|(&pid, info)| {
            let mut prep = vec![0x16u8]; // COM_STMT_PREPARE
            prep.extend_from_slice(&info.query);
            (pid, prep)
        }).collect()
    }
}

// ─── MySQL packet helpers ─────────────────────────────────────────────────────

/// Command bytes that carry a 4-byte stmt_id at bytes [1..5].
const STMT_ID_COMMANDS: &[u8] = &[
    0x17, // COM_STMT_EXECUTE
    0x19, // COM_STMT_CLOSE
    0x1A, // COM_STMT_RESET
    0x1C, // COM_STMT_FETCH
    0x18, // COM_STMT_SEND_LONG_DATA
];

/// Returns `true` when this command byte indicates a stmt_id at [1..5].
pub fn mysql_has_stmt_id(cmd_byte: u8) -> bool {
    STMT_ID_COMMANDS.contains(&cmd_byte)
}

/// Extract the 4-byte little-endian stmt_id from a `COM_STMT_*` packet.
pub fn mysql_read_stmt_id(packet: &[u8]) -> Option<u32> {
    if packet.len() < 5 { return None; }
    Some(u32::from_le_bytes([packet[1], packet[2], packet[3], packet[4]]))
}

/// Return a copy of `packet` with the stmt_id (bytes [1..5]) replaced by `new_id`.
pub fn mysql_rewrite_stmt_id(packet: &[u8], new_id: u32) -> Vec<u8> {
    let mut out = packet.to_vec();
    if out.len() >= 5 {
        out[1..5].copy_from_slice(&new_id.to_le_bytes());
    }
    out
}

/// Parse a `COM_STMT_PREPARE` OK response.
///
/// `response_bytes` starts with a 4-byte MySQL frame header (3-byte length + seq_id).
/// Returns `(stmt_id, num_columns, num_params)` or `None` on error / non-OK response.
pub fn mysql_parse_prepare_ok(response_bytes: &[u8]) -> Option<(u32, u16, u16)> {
    let payload = response_bytes.get(4..)?;         // skip 4-byte frame header
    if payload.first().copied() != Some(0x00) { return None; } // not OK
    if payload.len() < 9 { return None; }
    let stmt_id     = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
    let num_columns = u16::from_le_bytes([payload[5], payload[6]]);
    let num_params  = u16::from_le_bytes([payload[7], payload[8]]);
    Some((stmt_id, num_columns, num_params))
}

/// Rewrite the stmt_id inside a `COM_STMT_PREPARE` OK response **in-place**.
///
/// Layout: frame_header(4) + status(1) + stmt_id(4) + ... → stmt_id at [5..9].
pub fn mysql_rewrite_prepare_ok(response_bytes: &mut Vec<u8>, new_id: u32) {
    if response_bytes.len() >= 9 {
        response_bytes[5..9].copy_from_slice(&new_id.to_le_bytes());
    }
}
