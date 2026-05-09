//! Query rule engine — configurable routing rules that override the default
//! read/write splitting heuristic.
//!
//! Rules are evaluated in declaration order; the first match wins.
//! If no rule matches the caller falls back to the built-in heuristic.
//!
//! # Hot path
//! `match_query` holds the read-lock only long enough to clone the inner `Arc`
//! (a single pointer copy). The actual matching runs outside the lock — zero
//! contention with concurrent reloads.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use regex::Regex;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::config::{ProxyConfig, QueryRuleConfig, RuleDestination};

// ─── Public types ─────────────────────────────────────────────────────────────

/// Where to route the matched query.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Destination {
    /// Fall through to the built-in heuristic (SELECT → replica, else primary).
    Any,
    /// Always route to the primary (read-write) backend.
    Primary,
    /// Always route to a replica.
    Replica,
    /// Route to a specific backend by hostgroup index.
    /// `0` = primary, `1..=N` = replica[N-1].
    Hostgroup(usize),
}

/// Value returned when a rule matches.
pub struct RuleMatch {
    pub destination: Destination,
    /// 0 = do not cache; >0 = honour rule's TTL hint (cache with default TTL).
    pub cache_ttl_secs: u64,
    /// Mirror target address (fire-and-forget), if configured.
    pub mirror_to: Option<String>,
    /// True when the matched rule's QPS limit has been exceeded.
    /// The router must return an error to the client rather than executing
    /// the query.
    pub rate_limited: bool,
    /// True when this rule requests fast-forward mode for the matched query.
    /// The server should bypass routing, rewriting, caching, and analytics
    /// and forward directly to the primary.
    pub fast_forward: bool,
}

// ─── Internal ─────────────────────────────────────────────────────────────────

struct CompiledRule {
    pattern: Option<Regex>,
    digest: Option<String>,
    user: String,
    schema: String,
    destination: Destination,
    cache_ttl_secs: u64,
    comment: String,
    mirror_to: Option<String>,
    /// 0 = always apply; 1–100 = percentage of matching queries to apply this rule to.
    rollout_pct: u8,
    /// When true, log the match decision but don't apply the rule.
    dry_run: bool,
    /// When true, fast-forward this query (skip routing/analytics pipeline).
    fast_forward: bool,
    // ── Token-bucket rate limiter ─────────────────────────────────────────────
    /// QPS limit (0 = disabled).
    qps_limit: u32,
    /// Available tokens * 1000 (scaled to avoid floating point).
    /// Initialized to `qps_limit * 1000` (= 1 second burst).
    token_bucket: AtomicI64,
    /// Last token-refill time in milliseconds since Unix epoch.
    token_bucket_ms: AtomicU64,
    // Atomics allow mutation through a shared `Arc<Vec<CompiledRule>>`.
    hit_count: AtomicU64,
    last_match_secs: AtomicU64, // unix timestamp; 0 = never matched
}

// `Regex`, `AtomicU64`, `AtomicI64` are all `Send + Sync`.
unsafe impl Send for CompiledRule {}
unsafe impl Sync for CompiledRule {}

impl CompiledRule {
    /// Try to consume one token from the token bucket.
    /// Returns `true` when the query is within the QPS limit, `false` when
    /// the limit has been exceeded. Always returns `true` when `qps_limit == 0`.
    fn try_consume_token(&self) -> bool {
        if self.qps_limit == 0 {
            return true;
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let last_ms = self.token_bucket_ms.load(Ordering::Relaxed);
        let elapsed = now_ms.saturating_sub(last_ms);
        if elapsed >= 1 {
            // Race to refill — only one thread wins per millisecond.
            if self
                .token_bucket_ms
                .compare_exchange(last_ms, now_ms, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                let add =
                    ((elapsed * self.qps_limit as u64) / 1_000).min(self.qps_limit as u64) as i64;
                let current = self.token_bucket.load(Ordering::Relaxed);
                let burst = self.qps_limit as i64;
                let new_val = (current + add).min(burst);
                self.token_bucket.store(new_val, Ordering::Relaxed);
            }
        }
        // Consume one token.
        let remaining = self.token_bucket.fetch_sub(1, Ordering::Relaxed);
        if remaining <= 0 {
            // Undo to prevent tokens from going deeply negative.
            self.token_bucket.fetch_add(1, Ordering::Relaxed);
            false
        } else {
            true
        }
    }
}

// ─── Snapshot (API serialisation) ────────────────────────────────────────────

#[derive(Serialize)]
pub struct RuleStats {
    pub match_pattern: Option<String>,
    pub match_digest: Option<String>,
    pub user: String,
    pub schema: String,
    pub destination: String,
    pub destination_hostgroup: Option<usize>,
    pub cache_ttl_secs: u64,
    pub comment: String,
    pub hit_count: u64,
    pub mirror_to: Option<String>,
    pub rollout_pct: Option<u8>,
    /// ISO-8601 timestamp of the last match, or `null` if never matched.
    pub last_match: Option<String>,
    /// When `true` the rule is in dry-run mode: it logs but doesn't route.
    pub dry_run: bool,
    /// Configured QPS limit (`null` = unlimited).
    pub qps_limit: Option<u32>,
    /// When `true`, the matched query is fast-forwarded (bypasses pipeline).
    pub fast_forward: bool,
}

// ─── RuleEngine ───────────────────────────────────────────────────────────────

/// Thread-safe, hot-reloadable query routing rule set.
///
/// Cheap to `clone()` — all mutable state lives behind `Arc<RwLock<Arc<_>>>`.
#[derive(Clone)]
pub struct RuleEngine {
    /// Double-Arc: outer for the lock, inner to enable lock-free snapshots.
    inner: Arc<RwLock<Arc<Vec<CompiledRule>>>>,
    /// Path to the TOML config file — needed by `reload_from_file`.
    config_path: Arc<String>,
}

impl RuleEngine {
    /// Build from config. All regex patterns are compiled here; startup fails
    /// fast if any pattern is invalid.
    pub fn new(rules: &[QueryRuleConfig], config_path: impl Into<String>) -> anyhow::Result<Self> {
        let compiled = compile_rules(rules)?;
        Ok(Self {
            inner: Arc::new(RwLock::new(Arc::new(compiled))),
            config_path: Arc::new(config_path.into()),
        })
    }

    /// Re-read the config file and atomically swap the rule set.
    /// Hit counters from the previous set are discarded.
    pub async fn reload_from_file(&self) -> anyhow::Result<()> {
        let config = ProxyConfig::from_file(std::path::Path::new(self.config_path.as_str()))
            .context("reload_from_file: failed to parse config")?;
        let compiled = compile_rules(&config.query_rules)?;
        let count = compiled.len();
        {
            let mut guard = self.inner.write().await;
            *guard = Arc::new(compiled);
        }
        log::info!(
            "[rules] hot-reloaded {} rule(s) from {}",
            count,
            self.config_path
        );
        Ok(())
    }

    /// Atomically replace the rule set from an in-memory slice.
    /// Used by the runtime config store (Fase 0.5) to apply UI changes immediately.
    pub async fn reload_from_slice(&self, rules: &[QueryRuleConfig]) -> anyhow::Result<()> {
        let compiled = compile_rules(rules)?;
        let count = compiled.len();
        {
            let mut guard = self.inner.write().await;
            *guard = Arc::new(compiled);
        }
        log::info!(
            "[rules] applied {} rule(s) from runtime config store",
            count
        );
        Ok(())
    }

    /// Match `sql` against the rule set.
    ///
    /// Returns `None` when no rule matches — caller should apply its own
    /// heuristic. Never blocks on I/O.
    pub async fn match_query(&self, sql: &str, user: &str, schema: &str) -> Option<RuleMatch> {
        // Clone the Arc to release the lock before the (potentially slow)
        // fingerprint computation.
        let rules = {
            let guard = self.inner.read().await;
            Arc::clone(&*guard)
        };

        for rule in rules.iter() {
            // User filter (empty string = no filter).
            if !rule.user.is_empty() && rule.user != user {
                continue;
            }
            // Schema filter.
            if !rule.schema.is_empty() && rule.schema != schema {
                continue;
            }
            // Pattern / digest match — digest takes precedence.
            let matched = match (&rule.digest, &rule.pattern) {
                (Some(d), _) => {
                    let fp = crate::proxy::fingerprint::fingerprint(sql);
                    fp == *d
                }
                (None, Some(re)) => re.is_match(sql),
                (None, None) => false,
            };

            if matched {
                // Incremental rollout: only apply the rule to `rollout_pct` % of traffic.
                if rule.rollout_pct > 0 && rule.rollout_pct < 100 {
                    let roll: u8 = (rand::random::<u64>() % 100) as u8;
                    if roll >= rule.rollout_pct {
                        // This query is NOT in the rollout slice — skip rule, keep evaluating.
                        continue;
                    }
                }

                // Dry-run: log what would happen but don't apply the rule.
                if rule.dry_run {
                    log::info!(
                        "[rules:dry-run] rule {:?} matched (dest={:?}) for query: {}",
                        rule.comment,
                        rule.destination,
                        &sql[..sql.len().min(120)]
                    );
                    continue;
                }

                rule.hit_count.fetch_add(1, Ordering::Relaxed);
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                rule.last_match_secs.store(now, Ordering::Relaxed);

                // QPS rate limiting.
                if !rule.try_consume_token() {
                    log::warn!(
                        "[rules] QPS limit ({}/s) exceeded for rule {:?}",
                        rule.qps_limit,
                        rule.comment
                    );
                    return Some(RuleMatch {
                        destination: rule.destination,
                        cache_ttl_secs: 0,
                        mirror_to: None,
                        rate_limited: true,
                        fast_forward: false,
                    });
                }

                return Some(RuleMatch {
                    destination: rule.destination,
                    cache_ttl_secs: rule.cache_ttl_secs,
                    mirror_to: rule.mirror_to.clone(),
                    rate_limited: false,
                    fast_forward: rule.fast_forward,
                });
            }
        }
        None
    }

    /// Snapshot current rule stats for the dashboard API.
    pub async fn snapshot(&self) -> Vec<RuleStats> {
        let rules = {
            let guard = self.inner.read().await;
            Arc::clone(&*guard)
        };
        rules
            .iter()
            .map(|r| {
                let last_match = {
                    let ts = r.last_match_secs.load(Ordering::Relaxed);
                    if ts == 0 {
                        None
                    } else {
                        use chrono::{TimeZone, Utc};
                        Utc.timestamp_opt(ts as i64, 0)
                            .single()
                            .map(|dt| dt.to_rfc3339())
                    }
                };
                RuleStats {
                    match_pattern: r.pattern.as_ref().map(|re| re.as_str().to_string()),
                    match_digest: r.digest.clone(),
                    user: r.user.clone(),
                    schema: r.schema.clone(),
                    destination: match r.destination {
                        Destination::Any => "any".to_string(),
                        Destination::Primary => "primary".to_string(),
                        Destination::Replica => "replica".to_string(),
                        Destination::Hostgroup(n) => format!("hostgroup:{}", n),
                    },
                    destination_hostgroup: match r.destination {
                        Destination::Hostgroup(n) => Some(n),
                        _ => None,
                    },
                    cache_ttl_secs: r.cache_ttl_secs,
                    comment: r.comment.clone(),
                    hit_count: r.hit_count.load(Ordering::Relaxed),
                    mirror_to: r.mirror_to.clone(),
                    rollout_pct: if r.rollout_pct == 0 {
                        None
                    } else {
                        Some(r.rollout_pct)
                    },
                    last_match,
                    dry_run: r.dry_run,
                    qps_limit: if r.qps_limit == 0 {
                        None
                    } else {
                        Some(r.qps_limit)
                    },
                    fast_forward: r.fast_forward,
                }
            })
            .collect()
    }
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

fn compile_rules(configs: &[QueryRuleConfig]) -> anyhow::Result<Vec<CompiledRule>> {
    let mut out = Vec::with_capacity(configs.len());
    for (idx, cfg) in configs.iter().enumerate() {
        let pattern = cfg
            .match_pattern
            .as_deref()
            .map(|p| {
                Regex::new(p)
                    .with_context(|| format!("query_rules[{}]: invalid regex {:?}", idx, p))
            })
            .transpose()?;

        let destination = if let Some(hg) = cfg.destination_hostgroup {
            Destination::Hostgroup(hg as usize)
        } else {
            match cfg.destination {
                RuleDestination::Any => Destination::Any,
                RuleDestination::Primary => Destination::Primary,
                RuleDestination::Replica => Destination::Replica,
            }
        };

        out.push(CompiledRule {
            pattern,
            digest: cfg.match_digest.clone(),
            user: cfg.user.clone(),
            schema: cfg.schema.clone(),
            destination,
            cache_ttl_secs: cfg.cache_ttl_secs,
            comment: cfg.comment.clone(),
            mirror_to: cfg.mirror_to.clone(),
            rollout_pct: cfg.rollout_pct.unwrap_or(0).min(100),
            dry_run: cfg.dry_run,
            fast_forward: cfg.fast_forward,
            qps_limit: cfg.qps_limit.unwrap_or(0),
            // Initialize bucket to 1 second’s worth of tokens (burst capacity).
            token_bucket: AtomicI64::new(cfg.qps_limit.unwrap_or(0) as i64),
            token_bucket_ms: AtomicU64::new(0),
            hit_count: AtomicU64::new(0),
            last_match_secs: AtomicU64::new(0),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RuleDestination;

    fn make_engine(rules: Vec<QueryRuleConfig>) -> RuleEngine {
        RuleEngine::new(&rules, "/dev/null").unwrap()
    }

    fn base_rule() -> QueryRuleConfig {
        QueryRuleConfig {
            match_pattern: None,
            match_digest: None,
            user: String::new(),
            schema: String::new(),
            destination: RuleDestination::Replica,
            cache_ttl_secs: 0,
            comment: "test".to_string(),
            mirror_to: None,
            destination_hostgroup: None,
            rollout_pct: None,
            qps_limit: None,
            dry_run: false,
            fast_forward: false,
        }
    }

    // ── dry_run ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_dry_run_does_not_apply_rule() {
        let rule = QueryRuleConfig {
            match_pattern: Some("SELECT.*".to_string()),
            dry_run: true,
            ..base_rule()
        };
        let engine = make_engine(vec![rule]);
        // With dry_run=true the rule should not match (returns None).
        let result = engine.match_query("SELECT 1", "", "").await;
        assert!(result.is_none(), "dry_run rule must not affect routing");
    }

    #[tokio::test]
    async fn test_dry_run_falls_through_to_next_rule() {
        let dry = QueryRuleConfig {
            match_pattern: Some("SELECT.*".to_string()),
            destination: RuleDestination::Primary,
            dry_run: true,
            ..base_rule()
        };
        let real = QueryRuleConfig {
            match_pattern: Some("SELECT.*".to_string()),
            destination: RuleDestination::Replica,
            dry_run: false,
            ..base_rule()
        };
        let engine = make_engine(vec![dry, real]);
        let result = engine.match_query("SELECT 1", "", "").await.unwrap();
        // dry rule should be skipped; real rule (Replica) should match.
        assert_eq!(result.destination, Destination::Replica);
        assert!(!result.rate_limited);
    }

    #[tokio::test]
    async fn test_normal_rule_matches() {
        let rule = QueryRuleConfig {
            match_pattern: Some("SELECT.*".to_string()),
            destination: RuleDestination::Replica,
            ..base_rule()
        };
        let engine = make_engine(vec![rule]);
        let result = engine.match_query("SELECT 1", "", "").await.unwrap();
        assert_eq!(result.destination, Destination::Replica);
        assert!(!result.rate_limited);
    }

    // ── qps_limit / token bucket ──────────────────────────────────────────────

    #[test]
    fn test_token_bucket_allows_up_to_limit() {
        let rule = CompiledRule {
            pattern: None,
            digest: None,
            user: String::new(),
            schema: String::new(),
            destination: Destination::Replica,
            cache_ttl_secs: 0,
            comment: String::new(),
            mirror_to: None,
            rollout_pct: 0,
            dry_run: false,
            fast_forward: false,
            qps_limit: 5,
            token_bucket: AtomicI64::new(5),
            token_bucket_ms: AtomicU64::new(0),
            hit_count: AtomicU64::new(0),
            last_match_secs: AtomicU64::new(0),
        };
        // First 5 requests must succeed.
        for i in 0..5 {
            assert!(rule.try_consume_token(), "request {} should succeed", i);
        }
        // 6th request must be rejected.
        assert!(
            !rule.try_consume_token(),
            "request 6 should be rate-limited"
        );
    }

    #[test]
    fn test_token_bucket_unlimited_when_zero() {
        let rule = CompiledRule {
            pattern: None,
            digest: None,
            user: String::new(),
            schema: String::new(),
            destination: Destination::Replica,
            cache_ttl_secs: 0,
            comment: String::new(),
            mirror_to: None,
            rollout_pct: 0,
            dry_run: false,
            fast_forward: false,
            qps_limit: 0,
            token_bucket: AtomicI64::new(0),
            token_bucket_ms: AtomicU64::new(0),
            hit_count: AtomicU64::new(0),
            last_match_secs: AtomicU64::new(0),
        };
        for _ in 0..1000 {
            assert!(rule.try_consume_token());
        }
    }

    #[tokio::test]
    async fn test_rate_limited_flag_in_rule_match() {
        let rule = QueryRuleConfig {
            match_pattern: Some("SELECT.*".to_string()),
            qps_limit: Some(1),
            ..base_rule()
        };
        let engine = make_engine(vec![rule]);
        // First query: allowed.
        let first = engine.match_query("SELECT 1", "", "").await.unwrap();
        assert!(!first.rate_limited);
        // Second query (no time has passed): rate-limited.
        let second = engine.match_query("SELECT 1", "", "").await.unwrap();
        assert!(second.rate_limited);
    }

    // ── fast_forward per rule ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_fast_forward_rule_returns_flag() {
        let rule = QueryRuleConfig {
            match_pattern: Some("(?i)^SELECT 1$".to_string()),
            destination: RuleDestination::Primary,
            fast_forward: true,
            ..base_rule()
        };
        let engine = make_engine(vec![rule]);
        let m = engine.match_query("SELECT 1", "", "").await.unwrap();
        assert!(m.fast_forward, "fast_forward flag must be set in RuleMatch");
    }

    #[tokio::test]
    async fn test_normal_rule_has_no_fast_forward() {
        let rule = QueryRuleConfig {
            match_pattern: Some("(?i)^SELECT.*".to_string()),
            destination: RuleDestination::Replica,
            fast_forward: false,
            ..base_rule()
        };
        let engine = make_engine(vec![rule]);
        let m = engine.match_query("SELECT 1", "", "").await.unwrap();
        assert!(!m.fast_forward, "fast_forward must be false when not set");
    }

    #[tokio::test]
    async fn test_unmatched_rule_no_fast_forward() {
        let rule = QueryRuleConfig {
            match_pattern: Some("(?i)^DELETE".to_string()),
            destination: RuleDestination::Primary,
            fast_forward: true,
            ..base_rule()
        };
        let engine = make_engine(vec![rule]);
        // SELECT does not match the DELETE rule — must return None.
        let result = engine.match_query("SELECT 1", "", "").await;
        assert!(
            result.is_none(),
            "non-matching rule must not return RuleMatch"
        );
    }
}
