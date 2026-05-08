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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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
    // Atomics allow mutation through a shared `Arc<Vec<CompiledRule>>`.
    hit_count: AtomicU64,
    last_match_secs: AtomicU64, // unix timestamp; 0 = never matched
}

// `Regex` is `Send + Sync`; `AtomicU64` is `Send + Sync`.
unsafe impl Send for CompiledRule {}
unsafe impl Sync for CompiledRule {}

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
        log::info!("[rules] applied {} rule(s) from runtime config store", count);
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
                rule.hit_count.fetch_add(1, Ordering::Relaxed);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                rule.last_match_secs.store(now, Ordering::Relaxed);
                return Some(RuleMatch {
                    destination: rule.destination,
                    cache_ttl_secs: rule.cache_ttl_secs,
                    mirror_to: rule.mirror_to.clone(),
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
                    rollout_pct: if r.rollout_pct == 0 { None } else { Some(r.rollout_pct) },
                    last_match,
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
            hit_count: AtomicU64::new(0),
            last_match_secs: AtomicU64::new(0),
        });
    }
    Ok(out)
}
