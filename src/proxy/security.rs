//! Security features:
//!   - SQL injection pattern detection (classic and modern patterns)
//!   - Query fingerprint whitelist enforcement
//!   - Append-only NDJSON audit log

use std::collections::HashSet;
use std::io::Write;
use std::sync::{Arc, Mutex};

use crate::proxy::fingerprint::fingerprint;

// ─── SQL Injection detector ────────────────────────────────────────────────────

/// Compiled set of SQL injection patterns (case-insensitive regex).
pub struct InjectionDetector {
    patterns: Vec<regex::Regex>,
}

impl InjectionDetector {
    /// Build the detector with the built-in pattern library.
    pub fn new() -> Self {
        // Classic and modern SQLi patterns — ordered roughly by severity/frequency.
        let raw: &[&str] = &[
            // Tautologies: always-true conditions used to bypass WHERE filters.
            r#"(?i)\bOR\b\s+['""]?\d+['""]?\s*=\s*['""]?\d+['""]?"#,
            r#"(?i)\bAND\b\s+['""]?\d+['""]?\s*=\s*['""]?\d+['""]?"#,
            // Comment injection: -- and /* */ to truncate the rest of the query.
            r"(?i)--\s*$",
            r"(?i)/\*.*?\*/",
            // UNION-based injection.
            r"(?i)\bUNION\b\s+(ALL\s+)?\bSELECT\b",
            // Stacked queries / multiple statements.
            r"(?i);\s*(SELECT|INSERT|UPDATE|DELETE|DROP|CREATE|ALTER|EXEC|EXECUTE|CALL)\b",
            // Dangerous function calls.
            r"(?i)\bSLEEP\s*\(",
            r"(?i)\bBENCHMARK\s*\(",
            r"(?i)\bLOAD_FILE\s*\(",
            r"(?i)\bINTO\s+OUTFILE\b",
            r"(?i)\bINTO\s+DUMPFILE\b",
            // Information-schema / system table probing.
            r"(?i)\binformation_schema\s*\.",
            r"(?i)\bperformance_schema\s*\.",
            // Hex encoding tricks.
            r"(?i)\b0x[0-9a-fA-F]+",
            r"(?i)\bCHAR\s*\(",
            // EXEC / xp_cmdshell (MSSQL leftovers sometimes smuggled).
            r"(?i)\bxp_cmdshell\b",
            r"(?i)\bsp_executesql\b",
            // Blind injection timing patterns.
            r"(?i)\bWAITFOR\s+DELAY\b",
        ];

        let patterns = raw
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect();

        Self { patterns }
    }

    /// Returns the first matching pattern description, or `None` if clean.
    pub fn check(&self, sql: &str) -> Option<&str> {
        for re in &self.patterns {
            if re.is_match(sql) {
                return Some(re.as_str());
            }
        }
        None
    }
}

// ─── Query whitelist ───────────────────────────────────────────────────────────

/// Normalised-fingerprint allowlist.  When populated, any query whose
/// fingerprint is not in the set is rejected with MySQL error 1045.
#[derive(Clone)]
pub struct QueryWhitelist {
    fingerprints: Arc<HashSet<String>>,
}

impl QueryWhitelist {
    pub fn new(allowed: &[String]) -> Self {
        Self {
            fingerprints: Arc::new(allowed.iter().cloned().collect()),
        }
    }

    /// `true` when the list is empty (whitelist enforcement disabled).
    #[allow(dead_code)]
    pub fn is_disabled(&self) -> bool {
        self.fingerprints.is_empty()
    }

    /// `true` when `sql` is allowed (fingerprint is in the set, or the list is empty).
    pub fn is_allowed(&self, sql: &str) -> bool {
        if self.fingerprints.is_empty() {
            return true;
        }
        let fp = fingerprint(sql);
        self.fingerprints.contains(&fp)
    }
}

// ─── Audit logger ─────────────────────────────────────────────────────────────

/// Append-only NDJSON audit logger.
/// Each line is a JSON object:
/// `{"ts":"…","user":"…","client":"…","sql":"…","destination":"…","duration_ms":…,"error":false}`
///
/// Thread-safe: wraps a `Mutex<File>` so concurrent sessions can write safely.
/// File is re-opened on `reopen()` to support external log rotation (SIGHUP).
#[derive(Clone)]
pub struct AuditLogger {
    inner: Arc<Mutex<AuditLoggerInner>>,
}

struct AuditLoggerInner {
    #[allow(dead_code)]
    path: String,
    file: Option<std::fs::File>,
}

impl AuditLogger {
    /// Create with the given path.  If the path is empty, the logger is a no-op.
    pub fn new(path: &str) -> Self {
        let file = if path.is_empty() {
            None
        } else {
            match std::fs::OpenOptions::new().create(true).append(true).open(path) {
                Ok(f) => Some(f),
                Err(e) => {
                    log::error!("[audit] failed to open {}: {}", path, e);
                    None
                }
            }
        };
        Self {
            inner: Arc::new(Mutex::new(AuditLoggerInner {
                path: path.to_string(),
                file,
            })),
        }
    }

    /// `true` when logging is active (path was set and file opened successfully).
    pub fn is_active(&self) -> bool {
        self.inner.lock().unwrap().file.is_some()
    }

    /// Re-open the log file (call after log rotation / SIGHUP).
    #[allow(dead_code)]
    pub fn reopen(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.path.is_empty() {
            return;
        }
        match std::fs::OpenOptions::new().create(true).append(true).open(&inner.path) {
            Ok(f) => { inner.file = Some(f); }
            Err(e) => { log::error!("[audit] reopen {} failed: {}", inner.path, e); }
        }
    }

    /// Write one audit record.  Never blocks (fire-and-forget on lock failure).
    pub fn log(
        &self,
        user: &str,
        client: &str,
        sql: &str,
        destination: &str,
        duration_ms: f64,
        is_error: bool,
    ) {
        let mut inner = match self.inner.try_lock() {
            Ok(g) => g,
            Err(_) => return, // never block the query hot path
        };
        let Some(ref mut f) = inner.file else { return };

        // Minimal JSON serialisation — avoids pulling in serde_json on the hot path.
        // We only escape the characters that break JSON string safety.
        let safe_sql = sql.replace('\\', r"\\").replace('"', r#"\""#).replace('\n', r"\n");
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let line = format!(
            "{{\"ts\":\"{ts}\",\"user\":\"{user}\",\"client\":\"{client}\",\
             \"sql\":\"{safe_sql}\",\"destination\":\"{destination}\",\
             \"duration_ms\":{duration_ms:.2},\"error\":{is_error}}}\n",
        );
        let _ = f.write_all(line.as_bytes());
    }
}
