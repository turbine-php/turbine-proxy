//! Query rewriting engine — alters SQL text before it is dispatched to a backend.
//!
//! Rules are evaluated in declaration order; the first matching rule is applied
//! and no further rules are checked.  Rewrites run **before** routing so that
//! query-routing rules and the query cache see the final (rewritten) SQL.
//!
//! # Supported operations (per rule, in evaluation order)
//! 1. **Block** — reject the query and send an error to the client.
//! 2. **replace_with** — regex substitution on the raw SQL.
//! 3. **add_timeout_ms** — inject `/*+ MAX_EXECUTION_TIME(N) */` after `SELECT`.
//! 4. **add_limit** — append `LIMIT N` when the SELECT has no existing LIMIT.
//!
//! Multiple operations can be combined in one rule (e.g. replace + add_limit).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use regex::Regex;
use serde::Serialize;

use crate::config::QueryRewriteConfig;

// ─── Public outcome ───────────────────────────────────────────────────────────

/// Result of running a SQL string through the rewrite engine.
pub enum RewriteOutcome {
    /// No rule matched; SQL is unchanged.
    Unchanged,
    /// A rule matched and the SQL was transformed.
    Rewritten(String),
    /// A rule matched and explicitly blocked the query.
    Blocked(String),
}

// ─── Public stats snapshot ────────────────────────────────────────────────────

/// Dashboard-visible statistics for a single rewrite rule.
#[derive(Debug, Clone, Serialize)]
pub struct RewriteRuleStat {
    pub match_pattern: String,
    pub replace_with: Option<String>,
    pub add_limit: Option<u32>,
    pub add_timeout_ms: Option<u64>,
    pub block: bool,
    pub comment: String,
    pub hit_count: u64,
    /// Unix timestamp of the last hit, or `null` / 0 if never matched.
    pub last_match_secs: u64,
}

// ─── Internal compiled rule ───────────────────────────────────────────────────

struct CompiledRule {
    pattern: Regex,
    replace_with: Option<String>,
    add_limit: Option<u32>,
    add_timeout_ms: Option<u64>,
    block: bool,
    comment: String,
    hit_count: AtomicU64,
    last_match_secs: AtomicU64, // unix secs; 0 = never
}

// SAFETY: `Regex` is `Send + Sync`; `AtomicU64` is `Send + Sync`.
unsafe impl Send for CompiledRule {}
unsafe impl Sync for CompiledRule {}

impl CompiledRule {
    fn record_hit(&self) {
        self.hit_count.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_match_secs.store(now, Ordering::Relaxed);
    }
}

// ─── Rewriter ─────────────────────────────────────────────────────────────────

/// Hot-reloadable rewrite-rule set.
///
/// Build once at startup with [`Rewriter::new`], then share via `Arc<Rewriter>`.
/// Call [`Rewriter::reload_from_file`] to atomically swap the rule set at runtime
/// (e.g. on SIGHUP or via the dashboard `/api/reload` endpoint).
///
/// # Hot path
/// `apply()` and `is_empty()` acquire a `std::sync::RwLock` read lock, which is
/// held only for the duration of pattern matching — typically < 1 µs.  Write
/// locks are taken only during a reload (rare) and are therefore never contended
/// with each other.
pub struct Rewriter {
    /// Double-Arc: outer for shared ownership, inner for lock-free snapshots.
    inner: Arc<parking_lot::RwLock<Arc<Vec<CompiledRule>>>>,
    /// Path to the TOML config file — needed by `reload_from_file`.
    config_path: Arc<String>,
}

impl Rewriter {
    /// Compile all rules from configuration.  Returns an error if any regex
    /// fails to compile so that the proxy fails fast at startup.
    pub fn new(
        configs: &[QueryRewriteConfig],
        config_path: impl Into<String>,
    ) -> anyhow::Result<Arc<Self>> {
        let rules = compile_rules(configs)?;
        Ok(Arc::new(Self {
            inner: Arc::new(parking_lot::RwLock::new(Arc::new(rules))),
            config_path: Arc::new(config_path.into()),
        }))
    }

    /// Re-read the config file and atomically swap the rewrite rule set.
    /// Hit counters from the previous set are discarded.
    pub async fn reload_from_file(&self) -> anyhow::Result<()> {
        let path = self.config_path.as_str().to_string();
        let rules = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<CompiledRule>> {
            let config = crate::config::ProxyConfig::from_file(std::path::Path::new(&path))
                .context("reload_from_file: failed to parse config")?;
            compile_rules(&config.rewrite_rules)
        })
        .await
        .context("reload task panicked")??;
        let count = rules.len();
        {
            let mut guard = self.inner.write();
            *guard = Arc::new(rules);
        }
        log::info!(
            "[rewriter] hot-reloaded {} rule(s) from {}",
            count,
            self.config_path
        );
        Ok(())
    }

    /// Atomically replace the rule set from an in-memory slice.
    /// Used by the runtime config store (Fase 0.5) to apply UI changes immediately.
    pub fn reload_from_slice(
        &self,
        configs: &[crate::config::QueryRewriteConfig],
    ) -> anyhow::Result<()> {
        let rules = compile_rules(configs)?;
        let count = rules.len();
        {
            let mut guard = self.inner.write();
            *guard = Arc::new(rules);
        }
        log::info!(
            "[rewriter] applied {} rule(s) from runtime config store",
            count
        );
        Ok(())
    }

    /// Apply the rewrite engine to a SQL string.
    ///
    /// Returns [`RewriteOutcome::Unchanged`] when no rule matches.
    /// Only the first matching rule is applied.
    pub fn apply(&self, sql: &str) -> RewriteOutcome {
        // Clone the inner Arc to release the lock before pattern matching.
        let rules = {
            let guard = self.inner.read();
            Arc::clone(&*guard)
        };
        for rule in rules.iter() {
            if !rule.pattern.is_match(sql) {
                continue;
            }
            rule.record_hit();

            // 1. Block takes precedence over everything else.
            if rule.block {
                let msg = if rule.comment.is_empty() {
                    "Query blocked by rewrite rule".to_string()
                } else {
                    format!("Query blocked by rewrite rule: {}", rule.comment)
                };
                return RewriteOutcome::Blocked(msg);
            }

            let mut result = sql.to_string();

            // 2. Regex substitution.
            if let Some(ref replacement) = rule.replace_with {
                result = rule
                    .pattern
                    .replace_all(&result, replacement.as_str())
                    .into_owned();
            }

            // 3. MAX_EXECUTION_TIME hint injection (SELECT only).
            if let Some(ms) = rule.add_timeout_ms {
                result = inject_timeout_hint(&result, ms);
            }

            // 4. Force LIMIT (SELECT only, no existing LIMIT).
            if let Some(n) = rule.add_limit {
                result = enforce_limit(&result, n);
            }

            return RewriteOutcome::Rewritten(result);
        }

        RewriteOutcome::Unchanged
    }

    /// Return a snapshot of all rules with their current hit counters.
    pub fn snapshot(&self) -> Vec<RewriteRuleStat> {
        let rules = {
            let guard = self.inner.read();
            Arc::clone(&*guard)
        };
        rules
            .iter()
            .map(|r| RewriteRuleStat {
                match_pattern: r.pattern.as_str().to_string(),
                replace_with: r.replace_with.clone(),
                add_limit: r.add_limit,
                add_timeout_ms: r.add_timeout_ms,
                block: r.block,
                comment: r.comment.clone(),
                hit_count: r.hit_count.load(Ordering::Relaxed),
                last_match_secs: r.last_match_secs.load(Ordering::Relaxed),
            })
            .collect()
    }

    /// True when there are no compiled rules (fast-path skip in router).
    pub fn is_empty(&self) -> bool {
        let guard = self.inner.read();
        guard.is_empty()
    }
}

fn compile_rules(configs: &[QueryRewriteConfig]) -> anyhow::Result<Vec<CompiledRule>> {
    let mut rules = Vec::with_capacity(configs.len());
    for (i, cfg) in configs.iter().enumerate() {
        let pattern = Regex::new(&cfg.match_pattern)
            .with_context(|| format!("rewrite_rules[{}]: invalid match_pattern", i))?;
        rules.push(CompiledRule {
            pattern,
            replace_with: cfg.replace_with.clone().filter(|s| !s.is_empty()),
            add_limit: cfg.add_limit,
            add_timeout_ms: cfg.add_timeout_ms,
            block: cfg.block,
            comment: cfg.comment.clone(),
            hit_count: AtomicU64::new(0),
            last_match_secs: AtomicU64::new(0),
        });
    }
    Ok(rules)
}

// ─── Rewrite helpers ──────────────────────────────────────────────────────────

/// Inject `/*+ MAX_EXECUTION_TIME(ms) */` after the `SELECT` keyword.
///
/// Only applies when:
/// - The effective start of the query is `SELECT` (case-insensitive).
/// - The hint is not already present.
fn inject_timeout_hint(sql: &str, ms: u64) -> String {
    let trimmed = sql.trim_start();
    let upper = trimmed.to_uppercase();
    if !upper.starts_with("SELECT") {
        return sql.to_string();
    }
    if sql.contains("MAX_EXECUTION_TIME") {
        return sql.to_string();
    }
    let hint = format!("/*+ MAX_EXECUTION_TIME({}) */ ", ms);
    // Insert after "SELECT"
    let select_end = trimmed
        .find(|c: char| c.is_ascii_whitespace() || c == '/')
        .unwrap_or(6)
        .max(6); // at least past "SELECT"
    let (before, after) = trimmed.split_at(select_end);
    format!("{}{}{}", before, hint, after)
}

/// Append `LIMIT N` if the SQL is a SELECT that has no existing LIMIT clause.
///
/// Only applies when:
/// - The effective start of the query is `SELECT` (case-insensitive).
/// - The uppercase text does not already contain ` LIMIT `.
fn enforce_limit(sql: &str, n: u32) -> String {
    let trimmed = sql.trim_start();
    if !trimmed.to_uppercase().starts_with("SELECT") {
        return sql.to_string();
    }
    let upper = sql.to_uppercase();
    // Naive check: if " LIMIT " already appears anywhere, skip.
    if upper.contains(" LIMIT ") || upper.trim_end().ends_with("LIMIT") {
        return sql.to_string();
    }
    // Strip trailing semicolons before appending.
    let bare = sql.trim_end().trim_end_matches(';');
    format!("{} LIMIT {}", bare, n)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rule(pattern: &str) -> QueryRewriteConfig {
        QueryRewriteConfig {
            match_pattern: pattern.to_string(),
            replace_with: None,
            add_limit: None,
            add_timeout_ms: None,
            block: false,
            comment: String::new(),
        }
    }

    #[test]
    fn no_rules_unchanged() {
        let r = Rewriter::new(&[], "").unwrap();
        assert!(matches!(r.apply("SELECT 1"), RewriteOutcome::Unchanged));
    }

    #[test]
    fn block_rule() {
        let mut cfg = make_rule("(?i)DROP");
        cfg.block = true;
        let r = Rewriter::new(&[cfg], "").unwrap();
        assert!(matches!(
            r.apply("DROP TABLE t"),
            RewriteOutcome::Blocked(_)
        ));
    }

    #[test]
    fn add_limit_appended() {
        let mut cfg = make_rule("(?i)^SELECT");
        cfg.add_limit = Some(100);
        let r = Rewriter::new(&[cfg], "").unwrap();
        match r.apply("SELECT * FROM t") {
            RewriteOutcome::Rewritten(s) => assert!(s.ends_with("LIMIT 100"), "got: {s}"),
            _ => panic!("expected Rewritten"),
        }
    }

    #[test]
    fn add_limit_skipped_when_present() {
        let mut cfg = make_rule("(?i)^SELECT");
        cfg.add_limit = Some(100);
        let r = Rewriter::new(&[cfg], "").unwrap();
        match r.apply("SELECT * FROM t LIMIT 5") {
            RewriteOutcome::Rewritten(s) => assert!(!s.contains("LIMIT 100"), "got: {s}"),
            _ => panic!("expected Rewritten"),
        }
    }

    #[test]
    fn timeout_hint_injected() {
        let mut cfg = make_rule("(?i)^SELECT");
        cfg.add_timeout_ms = Some(5000);
        let r = Rewriter::new(&[cfg], "").unwrap();
        match r.apply("SELECT id FROM t") {
            RewriteOutcome::Rewritten(s) => {
                assert!(s.contains("MAX_EXECUTION_TIME(5000)"), "got: {s}");
            }
            _ => panic!("expected Rewritten"),
        }
    }

    #[test]
    fn regex_substitution() {
        let mut cfg = make_rule("(?i)SELECT \\*");
        cfg.replace_with = Some("SELECT id, name".to_string());
        let r = Rewriter::new(&[cfg], "").unwrap();
        match r.apply("SELECT * FROM users") {
            RewriteOutcome::Rewritten(s) => assert!(s.starts_with("SELECT id, name"), "got: {s}"),
            _ => panic!("expected Rewritten"),
        }
    }

    #[test]
    fn non_select_not_limited() {
        let mut cfg = make_rule(".*");
        cfg.add_limit = Some(10);
        let r = Rewriter::new(&[cfg], "").unwrap();
        match r.apply("INSERT INTO t VALUES (1)") {
            RewriteOutcome::Rewritten(s) => assert!(!s.contains("LIMIT"), "got: {s}"),
            _ => panic!("expected Rewritten"),
        }
    }
}
