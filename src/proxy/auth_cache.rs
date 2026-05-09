//! Credential cache for client authentication.
//!
//! Stores pre-computed password hashes (mysql_native_password SHA1 chain
//! and caching_sha2_password SHA256 chain) per user, so the proxy can verify
//! client auth tokens locally without opening a new backend connection on every
//! client reconnect.
//!
//! # Open mode
//! When `AuthCache::is_open()` returns true (no users configured), the proxy
//! accepts any username/password — backward-compatible for dev environments.
//!
//! # Verification
//! Uses the same challenge-response logic as MySQL's `mysql_native_password`:
//!   token = SHA1(password) XOR SHA1(challenge + SHA1(SHA1(password)))
//!
//! Both SHA1 and SHA256 paths are attempted so the cache works regardless of
//! which auth plugin the client claims to use.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;

use crate::config::UserConfig;
use crate::protocol::mysql::auth::{sha256_stage2, stage2_hash, Sha256Hash, Stage2Hash};

// ─── UserRules ────────────────────────────────────────────────────────────────

/// Rules applied to a specific user after successful authentication.
#[derive(Clone, Debug, serde::Serialize)]
pub struct UserRules {
    pub allow_writes: bool,
    pub max_connections: usize,
}

// ─── CachedEntry ─────────────────────────────────────────────────────────────

struct CachedEntry {
    stage2: Stage2Hash,
    sha256: Sha256Hash,
    rules: UserRules,
    expires: Instant,
}

// ─── AuthCache ────────────────────────────────────────────────────────────────

/// Thread-safe credential cache.
pub struct AuthCache {
    entries: Arc<RwLock<HashMap<String, CachedEntry>>>,
    /// True when no users are configured — open mode, no verification.
    open: bool,
}

impl AuthCache {
    /// Build a cache pre-populated from `[[users]]` config.
    /// If `users` is empty the cache operates in open mode.
    pub fn from_config(users: &[UserConfig], ttl_secs: u64) -> Self {
        let ttl = Duration::from_secs(ttl_secs);
        let open = users.is_empty();
        let expires = Instant::now() + ttl;

        let mut map = HashMap::with_capacity(users.len());
        for u in users {
            map.insert(
                u.name.clone(),
                CachedEntry {
                    stage2: stage2_hash(&u.resolved_password()),
                    sha256: sha256_stage2(&u.resolved_password()),
                    rules: UserRules {
                        allow_writes: u.allow_writes,
                        max_connections: u.max_connections,
                    },
                    expires,
                },
            );
        }

        Self {
            entries: Arc::new(RwLock::new(map)),
            open,
        }
    }

    /// Verify a client auth token against the cached credentials.
    ///
    /// Returns `Some(UserRules)` on success, `None` on unknown user or bad token.
    /// In open mode always returns `Some(UserRules { allow_writes: true, .. })`.
    pub async fn verify(
        &self,
        username: &str,
        challenge: &[u8],
        token: &[u8],
    ) -> Option<UserRules> {
        if self.open {
            return Some(UserRules {
                allow_writes: true,
                max_connections: 0,
            });
        }

        let entries = self.entries.read().await;
        let entry = entries.get(username)?;

        // Refresh expiry check — expired entries count as unknown.
        if entry.expires <= Instant::now() {
            return None;
        }

        // Empty token only matches empty password.
        if token.is_empty() {
            let empty_stage2 = stage2_hash("");
            return if entry.stage2 == empty_stage2 {
                Some(entry.rules.clone())
            } else {
                None
            };
        }

        let valid = crate::protocol::mysql::auth::verify(challenge, token, &entry.stage2)
            || crate::protocol::mysql::auth::verify_sha256(challenge, token, &entry.sha256);

        if valid {
            Some(entry.rules.clone())
        } else {
            None
        }
    }
}
