//! Runtime configuration store backed by SQLite.
//!
//! On first startup the store seeds itself from the TOML config; after that
//! it is the source of truth for query_rules, rewrite_rules, backends and users.
//! The TOML file continues to own infra settings (listen_addr, TLS, cluster).

use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::config::{
    BackendConfig, QueryRewriteConfig, QueryRuleConfig, RuleDestination, TlsMode, UserConfig,
};

// ─── Schema ───────────────────────────────────────────────────────────────────

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS config_rules (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    priority             INTEGER NOT NULL DEFAULT 0,
    match_pattern        TEXT,
    match_digest         TEXT,
    user                 TEXT NOT NULL DEFAULT '',
    schema_name          TEXT NOT NULL DEFAULT '',
    destination          TEXT NOT NULL DEFAULT 'any',
    destination_hostgroup INTEGER,
    cache_ttl_secs       INTEGER NOT NULL DEFAULT 0,
    comment              TEXT NOT NULL DEFAULT '',
    mirror_to            TEXT,
    rollout_pct          INTEGER,
    enabled              INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS config_rewrite_rules (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    priority       INTEGER NOT NULL DEFAULT 0,
    match_pattern  TEXT NOT NULL,
    replace_with   TEXT,
    add_limit      INTEGER,
    add_timeout_ms INTEGER,
    block          INTEGER NOT NULL DEFAULT 0,
    comment        TEXT NOT NULL DEFAULT '',
    enabled        INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS config_backends (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    protocol  TEXT NOT NULL DEFAULT 'mysql',
    addr      TEXT NOT NULL,
    user      TEXT NOT NULL DEFAULT '',
    password  TEXT NOT NULL DEFAULT '',
    database  TEXT,
    role      TEXT NOT NULL DEFAULT 'replica',
    weight    INTEGER NOT NULL DEFAULT 100,
    backup    INTEGER NOT NULL DEFAULT 0,
    tls_mode  TEXT NOT NULL DEFAULT 'off',
    enabled   INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS config_users (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    name             TEXT NOT NULL UNIQUE,
    password         TEXT NOT NULL DEFAULT '',
    allow_writes     INTEGER NOT NULL DEFAULT 1,
    max_connections  INTEGER NOT NULL DEFAULT 0,
    enabled          INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS config_changes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    entity      TEXT NOT NULL,
    entity_id   INTEGER,
    action      TEXT NOT NULL,
    before_json TEXT,
    after_json  TEXT,
    author_ip   TEXT NOT NULL DEFAULT ''
);
";

// ─── API types (serialisable) ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleRow {
    pub id: i64,
    pub priority: i64,
    pub match_pattern: Option<String>,
    pub match_digest: Option<String>,
    pub user: String,
    pub schema_name: String,
    pub destination: String,
    pub destination_hostgroup: Option<i64>,
    pub cache_ttl_secs: i64,
    pub comment: String,
    pub mirror_to: Option<String>,
    pub rollout_pct: Option<i64>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteRuleRow {
    pub id: i64,
    pub priority: i64,
    pub match_pattern: String,
    pub replace_with: Option<String>,
    pub add_limit: Option<i64>,
    pub add_timeout_ms: Option<i64>,
    pub block: bool,
    pub comment: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendRow {
    pub id: i64,
    pub addr: String,
    pub user: String,
    /// Never exposed via API — always returned as `"***"`.
    #[serde(skip_serializing)]
    pub password: String,
    pub database: Option<String>,
    pub role: String,
    pub weight: i64,
    pub backup: bool,
    pub tls_mode: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRow {
    pub id: i64,
    pub name: String,
    /// Never exposed via API.
    #[serde(skip_serializing)]
    pub password: String,
    pub allow_writes: bool,
    pub max_connections: i64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRow {
    pub id: i64,
    pub ts: String,
    pub entity: String,
    pub entity_id: Option<i64>,
    pub action: String,
    pub before_json: Option<String>,
    pub after_json: Option<String>,
    pub author_ip: String,
}

// ─── ConfigStore ──────────────────────────────────────────────────────────────

pub struct ConfigStore {
    conn: Mutex<Connection>,
}

impl ConfigStore {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Opening config DB at '{db_path}'"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(SCHEMA)?;
        // Forward-compatible migration for older DBs created before `protocol`.
        if conn
            .execute(
                "ALTER TABLE config_backends ADD COLUMN protocol TEXT NOT NULL DEFAULT 'mysql'",
                [],
            )
            .is_ok()
        {
            log::info!("[config-store] migrated config_backends: added protocol column");
        }
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ── Seeding ───────────────────────────────────────────────────────────────

    /// Populate tables from TOML config if they are empty (first startup).
    pub fn seed_if_empty(
        &self,
        rules: &[QueryRuleConfig],
        rewrite_rules: &[QueryRewriteConfig],
        primary: &BackendConfig,
        replicas: &[BackendConfig],
        users: &[UserConfig],
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        let rules_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM config_rules", [], |r| r.get(0))?;
        if rules_count == 0 {
            for (i, r) in rules.iter().enumerate() {
                let dest = match r.destination {
                    RuleDestination::Any => "any",
                    RuleDestination::Primary => "primary",
                    RuleDestination::Replica => "replica",
                };
                conn.execute(
                    "INSERT INTO config_rules
                     (priority, match_pattern, match_digest, user, schema_name,
                      destination, destination_hostgroup, cache_ttl_secs,
                      comment, mirror_to, rollout_pct)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        i as i64,
                        r.match_pattern,
                        r.match_digest,
                        r.user,
                        r.schema,
                        dest,
                        r.destination_hostgroup.map(|v| v as i64),
                        r.cache_ttl_secs as i64,
                        r.comment,
                        r.mirror_to,
                        r.rollout_pct.map(|v| v as i64),
                    ],
                )?;
            }
        }

        let rr_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM config_rewrite_rules", [], |r| {
                r.get(0)
            })?;
        if rr_count == 0 {
            for (i, r) in rewrite_rules.iter().enumerate() {
                conn.execute(
                    "INSERT INTO config_rewrite_rules
                     (priority, match_pattern, replace_with, add_limit, add_timeout_ms, block, comment)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        i as i64,
                        r.match_pattern,
                        r.replace_with,
                        r.add_limit.map(|v| v as i64),
                        r.add_timeout_ms.map(|v| v as i64),
                        r.block as i64,
                        r.comment,
                    ],
                )?;
            }
        }

        let be_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM config_backends WHERE protocol='mysql'",
            [],
            |r| r.get(0),
        )?;
        if be_count == 0 {
            Self::insert_backend_inner(&conn, primary, "primary", "mysql")?;
            for r in replicas {
                Self::insert_backend_inner(&conn, r, "replica", "mysql")?;
            }
        }

        let u_count: i64 = conn.query_row("SELECT COUNT(*) FROM config_users", [], |r| r.get(0))?;
        if u_count == 0 {
            for u in users {
                conn.execute(
                    "INSERT OR IGNORE INTO config_users
                     (name, password, allow_writes, max_connections)
                     VALUES (?1,?2,?3,?4)",
                    params![
                        u.name,
                        u.password,
                        u.allow_writes as i64,
                        u.max_connections as i64
                    ],
                )?;
            }
        }

        Ok(())
    }

    // ── Query Rules ───────────────────────────────────────────────────────────

    pub fn list_rules(&self) -> Result<Vec<RuleRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,priority,match_pattern,match_digest,user,schema_name,
                    destination,destination_hostgroup,cache_ttl_secs,
                    comment,mirror_to,rollout_pct,enabled
             FROM config_rules ORDER BY priority, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RuleRow {
                id: r.get(0)?,
                priority: r.get(1)?,
                match_pattern: r.get(2)?,
                match_digest: r.get(3)?,
                user: r.get(4)?,
                schema_name: r.get(5)?,
                destination: r.get(6)?,
                destination_hostgroup: r.get(7)?,
                cache_ttl_secs: r.get(8)?,
                comment: r.get(9)?,
                mirror_to: r.get(10)?,
                rollout_pct: r.get(11)?,
                enabled: r.get::<_, i64>(12)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn create_rule(&self, row: &RuleRow, author_ip: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO config_rules
             (priority,match_pattern,match_digest,user,schema_name,
              destination,destination_hostgroup,cache_ttl_secs,
              comment,mirror_to,rollout_pct,enabled)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                row.priority,
                row.match_pattern,
                row.match_digest,
                row.user,
                row.schema_name,
                row.destination,
                row.destination_hostgroup,
                row.cache_ttl_secs,
                row.comment,
                row.mirror_to,
                row.rollout_pct,
                row.enabled as i64,
            ],
        )?;
        let id = conn.last_insert_rowid();
        Self::log_change_inner(
            &conn,
            "rule",
            Some(id),
            "create",
            None,
            Some(&serde_json::to_string(row)?),
            author_ip,
        )?;
        Ok(id)
    }

    pub fn update_rule(&self, id: i64, row: &RuleRow, author_ip: &str) -> Result<()> {
        let before = self.get_rule(id)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE config_rules SET
               priority=?1, match_pattern=?2, match_digest=?3, user=?4,
               schema_name=?5, destination=?6, destination_hostgroup=?7,
               cache_ttl_secs=?8, comment=?9, mirror_to=?10,
               rollout_pct=?11, enabled=?12
             WHERE id=?13",
            params![
                row.priority,
                row.match_pattern,
                row.match_digest,
                row.user,
                row.schema_name,
                row.destination,
                row.destination_hostgroup,
                row.cache_ttl_secs,
                row.comment,
                row.mirror_to,
                row.rollout_pct,
                row.enabled as i64,
                id,
            ],
        )?;
        Self::log_change_inner(
            &conn,
            "rule",
            Some(id),
            "update",
            before
                .as_ref()
                .map(|r| serde_json::to_string(r).unwrap_or_default())
                .as_deref(),
            Some(&serde_json::to_string(row)?),
            author_ip,
        )?;
        Ok(())
    }

    pub fn delete_rule(&self, id: i64, author_ip: &str) -> Result<()> {
        let before = self.get_rule(id)?;
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM config_rules WHERE id=?1", params![id])?;
        Self::log_change_inner(
            &conn,
            "rule",
            Some(id),
            "delete",
            before
                .as_ref()
                .map(|r| serde_json::to_string(r).unwrap_or_default())
                .as_deref(),
            None,
            author_ip,
        )?;
        Ok(())
    }

    pub fn get_rule(&self, id: i64) -> Result<Option<RuleRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,priority,match_pattern,match_digest,user,schema_name,
                    destination,destination_hostgroup,cache_ttl_secs,
                    comment,mirror_to,rollout_pct,enabled
             FROM config_rules WHERE id=?1",
        )?;
        let mut rows = stmt.query_map(params![id], |r| {
            Ok(RuleRow {
                id: r.get(0)?,
                priority: r.get(1)?,
                match_pattern: r.get(2)?,
                match_digest: r.get(3)?,
                user: r.get(4)?,
                schema_name: r.get(5)?,
                destination: r.get(6)?,
                destination_hostgroup: r.get(7)?,
                cache_ttl_secs: r.get(8)?,
                comment: r.get(9)?,
                mirror_to: r.get(10)?,
                rollout_pct: r.get(11)?,
                enabled: r.get::<_, i64>(12)? != 0,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    // ── Rewrite Rules ─────────────────────────────────────────────────────────

    pub fn list_rewrite_rules(&self) -> Result<Vec<RewriteRuleRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,priority,match_pattern,replace_with,add_limit,
                    add_timeout_ms,block,comment,enabled
             FROM config_rewrite_rules ORDER BY priority, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RewriteRuleRow {
                id: r.get(0)?,
                priority: r.get(1)?,
                match_pattern: r.get(2)?,
                replace_with: r.get(3)?,
                add_limit: r.get(4)?,
                add_timeout_ms: r.get(5)?,
                block: r.get::<_, i64>(6)? != 0,
                comment: r.get(7)?,
                enabled: r.get::<_, i64>(8)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn create_rewrite_rule(&self, row: &RewriteRuleRow, author_ip: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO config_rewrite_rules
             (priority,match_pattern,replace_with,add_limit,add_timeout_ms,block,comment,enabled)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                row.priority,
                row.match_pattern,
                row.replace_with,
                row.add_limit,
                row.add_timeout_ms,
                row.block as i64,
                row.comment,
                row.enabled as i64,
            ],
        )?;
        let id = conn.last_insert_rowid();
        Self::log_change_inner(
            &conn,
            "rewrite_rule",
            Some(id),
            "create",
            None,
            Some(&serde_json::to_string(row)?),
            author_ip,
        )?;
        Ok(id)
    }

    pub fn update_rewrite_rule(
        &self,
        id: i64,
        row: &RewriteRuleRow,
        author_ip: &str,
    ) -> Result<()> {
        let before = self.get_rewrite_rule(id)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE config_rewrite_rules SET
               priority=?1, match_pattern=?2, replace_with=?3, add_limit=?4,
               add_timeout_ms=?5, block=?6, comment=?7, enabled=?8
             WHERE id=?9",
            params![
                row.priority,
                row.match_pattern,
                row.replace_with,
                row.add_limit,
                row.add_timeout_ms,
                row.block as i64,
                row.comment,
                row.enabled as i64,
                id,
            ],
        )?;
        Self::log_change_inner(
            &conn,
            "rewrite_rule",
            Some(id),
            "update",
            before
                .as_ref()
                .map(|r| serde_json::to_string(r).unwrap_or_default())
                .as_deref(),
            Some(&serde_json::to_string(row)?),
            author_ip,
        )?;
        Ok(())
    }

    pub fn delete_rewrite_rule(&self, id: i64, author_ip: &str) -> Result<()> {
        let before = self.get_rewrite_rule(id)?;
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM config_rewrite_rules WHERE id=?1", params![id])?;
        Self::log_change_inner(
            &conn,
            "rewrite_rule",
            Some(id),
            "delete",
            before
                .as_ref()
                .map(|r| serde_json::to_string(r).unwrap_or_default())
                .as_deref(),
            None,
            author_ip,
        )?;
        Ok(())
    }

    pub fn get_rewrite_rule(&self, id: i64) -> Result<Option<RewriteRuleRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,priority,match_pattern,replace_with,add_limit,
                    add_timeout_ms,block,comment,enabled
             FROM config_rewrite_rules WHERE id=?1",
        )?;
        let mut rows = stmt.query_map(params![id], |r| {
            Ok(RewriteRuleRow {
                id: r.get(0)?,
                priority: r.get(1)?,
                match_pattern: r.get(2)?,
                replace_with: r.get(3)?,
                add_limit: r.get(4)?,
                add_timeout_ms: r.get(5)?,
                block: r.get::<_, i64>(6)? != 0,
                comment: r.get(7)?,
                enabled: r.get::<_, i64>(8)? != 0,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    // ── Backends ──────────────────────────────────────────────────────────────

    pub fn list_backends(&self) -> Result<Vec<BackendRow>> {
        self.list_backends_by_protocol("mysql")
    }

    pub fn list_backends_by_protocol(&self, protocol: &str) -> Result<Vec<BackendRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,addr,user,password,database,role,weight,backup,tls_mode,enabled
             FROM config_backends
             WHERE protocol=?1
             ORDER BY role DESC, id",
        )?;
        let rows = stmt.query_map(params![protocol], |r| {
            Ok(BackendRow {
                id: r.get(0)?,
                addr: r.get(1)?,
                user: r.get(2)?,
                password: r.get(3)?,
                database: r.get(4)?,
                role: r.get(5)?,
                weight: r.get(6)?,
                backup: r.get::<_, i64>(7)? != 0,
                tls_mode: r.get(8)?,
                enabled: r.get::<_, i64>(9)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn create_backend(&self, row: &BackendRow, author_ip: &str) -> Result<i64> {
        self.create_backend_with_protocol(row, author_ip, "mysql")
    }

    pub fn create_backend_with_protocol(
        &self,
        row: &BackendRow,
        author_ip: &str,
        protocol: &str,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO config_backends
             (protocol,addr,user,password,database,role,weight,backup,tls_mode,enabled)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                protocol,
                row.addr,
                row.user,
                row.password,
                row.database,
                row.role,
                row.weight,
                row.backup as i64,
                row.tls_mode,
                row.enabled as i64,
            ],
        )?;
        let id = conn.last_insert_rowid();
        // Log without password
        let safe = serde_json::json!({ "protocol": protocol, "addr": row.addr, "role": row.role, "weight": row.weight });
        Self::log_change_inner(
            &conn,
            "backend",
            Some(id),
            "create",
            None,
            Some(&safe.to_string()),
            author_ip,
        )?;
        Ok(id)
    }

    pub fn update_backend(&self, id: i64, row: &BackendRow, author_ip: &str) -> Result<()> {
        self.update_backend_with_protocol(id, row, author_ip, "mysql")
    }

    pub fn update_backend_with_protocol(
        &self,
        id: i64,
        row: &BackendRow,
        author_ip: &str,
        protocol: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE config_backends SET
               addr=?1, user=?2, password=?3, database=?4, role=?5,
               weight=?6, backup=?7, tls_mode=?8, enabled=?9
             WHERE id=?10 AND protocol=?11",
            params![
                row.addr,
                row.user,
                row.password,
                row.database,
                row.role,
                row.weight,
                row.backup as i64,
                row.tls_mode,
                row.enabled as i64,
                id,
                protocol,
            ],
        )?;
        let safe = serde_json::json!({ "protocol": protocol, "addr": row.addr, "role": row.role, "weight": row.weight, "enabled": row.enabled });
        Self::log_change_inner(
            &conn,
            "backend",
            Some(id),
            "update",
            None,
            Some(&safe.to_string()),
            author_ip,
        )?;
        Ok(())
    }

    pub fn delete_backend(&self, id: i64, author_ip: &str) -> Result<()> {
        self.delete_backend_with_protocol(id, author_ip, "mysql")
    }

    pub fn delete_backend_with_protocol(
        &self,
        id: i64,
        author_ip: &str,
        protocol: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM config_backends WHERE id=?1 AND protocol=?2",
            params![id, protocol],
        )?;
        Self::log_change_inner(&conn, "backend", Some(id), "delete", None, None, author_ip)?;
        Ok(())
    }

    // ── Users ─────────────────────────────────────────────────────────────────

    pub fn list_users(&self) -> Result<Vec<UserRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,name,password,allow_writes,max_connections,enabled
             FROM config_users ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(UserRow {
                id: r.get(0)?,
                name: r.get(1)?,
                password: r.get(2)?,
                allow_writes: r.get::<_, i64>(3)? != 0,
                max_connections: r.get(4)?,
                enabled: r.get::<_, i64>(5)? != 0,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn create_user(&self, row: &UserRow, author_ip: &str) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO config_users (name,password,allow_writes,max_connections,enabled)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                row.name,
                row.password,
                row.allow_writes as i64,
                row.max_connections,
                row.enabled as i64,
            ],
        )?;
        let id = conn.last_insert_rowid();
        let safe = serde_json::json!({ "name": row.name, "allow_writes": row.allow_writes });
        Self::log_change_inner(
            &conn,
            "user",
            Some(id),
            "create",
            None,
            Some(&safe.to_string()),
            author_ip,
        )?;
        Ok(id)
    }

    pub fn update_user(&self, id: i64, row: &UserRow, author_ip: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE config_users SET
               name=?1, password=?2, allow_writes=?3, max_connections=?4, enabled=?5
             WHERE id=?6",
            params![
                row.name,
                row.password,
                row.allow_writes as i64,
                row.max_connections,
                row.enabled as i64,
                id,
            ],
        )?;
        let safe = serde_json::json!({ "name": row.name, "allow_writes": row.allow_writes, "enabled": row.enabled });
        Self::log_change_inner(
            &conn,
            "user",
            Some(id),
            "update",
            None,
            Some(&safe.to_string()),
            author_ip,
        )?;
        Ok(())
    }

    pub fn delete_user(&self, id: i64, author_ip: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM config_users WHERE id=?1", params![id])?;
        Self::log_change_inner(&conn, "user", Some(id), "delete", None, None, author_ip)?;
        Ok(())
    }

    // ── Config history ────────────────────────────────────────────────────────

    pub fn list_changes(&self, limit: i64) -> Result<Vec<ChangeRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id,ts,entity,entity_id,action,before_json,after_json,author_ip
             FROM config_changes ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(ChangeRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                entity: r.get(2)?,
                entity_id: r.get(3)?,
                action: r.get(4)?,
                before_json: r.get(5)?,
                after_json: r.get(6)?,
                author_ip: r.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    // ── Conversion to config types (for hot-reload) ───────────────────────────

    /// Return enabled query rules as `QueryRuleConfig` slices for hot-reload.
    pub fn active_query_rules(&self) -> Result<Vec<QueryRuleConfig>> {
        let rows = self.list_rules()?;
        Ok(rows
            .into_iter()
            .filter(|r| r.enabled)
            .map(|r| QueryRuleConfig {
                match_pattern: r.match_pattern,
                match_digest: r.match_digest,
                user: r.user,
                schema: r.schema_name,
                destination: match r.destination.as_str() {
                    "primary" => RuleDestination::Primary,
                    "replica" => RuleDestination::Replica,
                    _ => RuleDestination::Any,
                },
                cache_ttl_secs: r.cache_ttl_secs as u64,
                comment: r.comment,
                mirror_to: r.mirror_to,
                destination_hostgroup: r.destination_hostgroup.map(|v| v as u32),
                rollout_pct: r.rollout_pct.map(|v| v as u8),
                qps_limit: None,
                dry_run: false,
                fast_forward: false,
            })
            .collect())
    }

    /// Return enabled rewrite rules as `QueryRewriteConfig` slices for hot-reload.
    pub fn active_rewrite_rules(&self) -> Result<Vec<QueryRewriteConfig>> {
        let rows = self.list_rewrite_rules()?;
        Ok(rows
            .into_iter()
            .filter(|r| r.enabled)
            .map(|r| QueryRewriteConfig {
                match_pattern: r.match_pattern,
                replace_with: r.replace_with,
                add_limit: r.add_limit.map(|v| v as u32),
                add_timeout_ms: r.add_timeout_ms.map(|v| v as u64),
                block: r.block,
                comment: r.comment,
            })
            .collect())
    }

    /// Return enabled backends as `BackendConfig` pairs (primary, replicas).
    pub fn active_backends(&self) -> Result<(Option<BackendConfig>, Vec<BackendConfig>)> {
        self.active_backends_for_protocol("mysql")
    }

    pub fn active_backends_for_protocol(
        &self,
        protocol: &str,
    ) -> Result<(Option<BackendConfig>, Vec<BackendConfig>)> {
        let rows = self.list_backends_by_protocol(protocol)?;
        let mut primary = None;
        let mut replicas = Vec::new();
        for row in rows.into_iter().filter(|r| r.enabled) {
            let cfg = BackendConfig {
                addr: row.addr,
                user: row.user,
                password: row.password,
                database: row.database,
                tls_mode: match row.tls_mode.as_str() {
                    "required" => TlsMode::Required,
                    "verify-ca" => TlsMode::VerifyCa,
                    "verify-identity" => TlsMode::VerifyIdentity,
                    _ => TlsMode::Off,
                },
                tls_ca: None,
                tls_cert: None,
                tls_key: None,
                weight: row.weight as u32,
                backup: row.backup,
                init_connect: Vec::new(),
                resolution_family: "system".to_string(),
                compression: crate::config::BackendCompression::None,
                ssl_keylog_file: String::new(),
                max_connections: None,
            };
            if row.role == "primary" {
                primary = Some(cfg);
            } else {
                replicas.push(cfg);
            }
        }
        Ok((primary, replicas))
    }

    /// Return enabled users as `UserConfig` slice.
    pub fn active_users(&self) -> Result<Vec<UserConfig>> {
        let rows = self.list_users()?;
        Ok(rows
            .into_iter()
            .filter(|r| r.enabled)
            .map(|r| UserConfig {
                name: r.name,
                password: r.password,
                allow_writes: r.allow_writes,
                max_connections: r.max_connections as usize,
                default_schema: String::new(),
                transaction_isolation: String::new(),
            })
            .collect())
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn insert_backend_inner(
        conn: &Connection,
        cfg: &BackendConfig,
        role: &str,
        protocol: &str,
    ) -> Result<()> {
        let tls = match cfg.tls_mode {
            TlsMode::Off => "off",
            TlsMode::Required => "required",
            TlsMode::VerifyCa => "verify-ca",
            TlsMode::VerifyIdentity => "verify-identity",
        };
        conn.execute(
            "INSERT INTO config_backends (protocol,addr,user,password,database,role,weight,backup,tls_mode)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                protocol,
                cfg.addr,
                cfg.user,
                cfg.password,
                cfg.database,
                role,
                cfg.weight as i64,
                cfg.backup as i64,
                tls,
            ],
        )?;
        Ok(())
    }

    fn log_change_inner(
        conn: &Connection,
        entity: &str,
        entity_id: Option<i64>,
        action: &str,
        before_json: Option<&str>,
        after_json: Option<&str>,
        author_ip: &str,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO config_changes (entity,entity_id,action,before_json,after_json,author_ip)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                entity,
                entity_id,
                action,
                before_json,
                after_json,
                author_ip
            ],
        )?;
        Ok(())
    }

    // ── Bulk replace (used by import) ─────────────────────────────────────────

    /// Delete all runtime config rows and re-seed from the given config structs.
    /// Infrastructure tables ([proxy], [tls]) are never touched.
    /// A single `config_changes` row is written for audit.
    pub fn replace_all(
        &self,
        rules: &[QueryRuleConfig],
        rewrite_rules: &[QueryRewriteConfig],
        primary: &BackendConfig,
        replicas: &[BackendConfig],
        users: &[UserConfig],
        author_ip: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute_batch(
            "
            DELETE FROM config_rules;
            DELETE FROM config_rewrite_rules;
            DELETE FROM config_backends WHERE protocol='mysql';
            DELETE FROM config_users;
        ",
        )?;

        for (i, r) in rules.iter().enumerate() {
            let dest = match r.destination {
                RuleDestination::Any => "any",
                RuleDestination::Primary => "primary",
                RuleDestination::Replica => "replica",
            };
            conn.execute(
                "INSERT INTO config_rules
                 (priority, match_pattern, match_digest, user, schema_name,
                  destination, destination_hostgroup, cache_ttl_secs,
                  comment, mirror_to, rollout_pct)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    i as i64,
                    r.match_pattern,
                    r.match_digest,
                    r.user,
                    r.schema,
                    dest,
                    r.destination_hostgroup.map(|v| v as i64),
                    r.cache_ttl_secs as i64,
                    r.comment,
                    r.mirror_to,
                    r.rollout_pct.map(|v| v as i64),
                ],
            )?;
        }

        for (i, r) in rewrite_rules.iter().enumerate() {
            conn.execute(
                "INSERT INTO config_rewrite_rules
                 (priority, match_pattern, replace_with, add_limit, add_timeout_ms, block, comment)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    i as i64,
                    r.match_pattern,
                    r.replace_with,
                    r.add_limit.map(|v| v as i64),
                    r.add_timeout_ms.map(|v| v as i64),
                    r.block as i64,
                    r.comment,
                ],
            )?;
        }

        Self::insert_backend_inner(&conn, primary, "primary", "mysql")?;
        for r in replicas {
            Self::insert_backend_inner(&conn, r, "replica", "mysql")?;
        }

        for u in users {
            conn.execute(
                "INSERT INTO config_users (name, password, allow_writes, max_connections)
                 VALUES (?1,?2,?3,?4)",
                params![
                    u.name,
                    u.password,
                    u.allow_writes as i64,
                    u.max_connections as i64,
                ],
            )?;
        }

        Self::log_change_inner(
            &conn,
            "all",
            None,
            "import",
            None,
            Some(&format!(
                r#"{{"rules":{},"rewrite_rules":{},"backends":{},"users":{}}}"#,
                rules.len(),
                rewrite_rules.len(),
                1 + replicas.len(),
                users.len()
            )),
            author_ip,
        )?;
        Ok(())
    }
}
