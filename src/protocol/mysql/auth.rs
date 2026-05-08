//! MySQL authentication helpers (`mysql_native_password` and `caching_sha2_password`).
//!
//! The proxy uses these to verify client credentials before forwarding to the backend,
//! or to perform pass-through authentication by relaying the challenge/response.
#![allow(unused)]

use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;

pub type Stage2Hash = [u8; 20];
pub type Sha256Hash = [u8; 32];

pub fn stage2_hash(password: &str) -> Stage2Hash {
    let mut h = Sha1::new();
    h.update(password.as_bytes());
    let stage1: [u8; 20] = h.finalize().into();
    let mut h = Sha1::new();
    h.update(stage1);
    h.finalize().into()
}

pub fn sha256_stage2(password: &str) -> Sha256Hash {
    let mut h = Sha256::new();
    h.update(password.as_bytes());
    let stage1: [u8; 32] = h.finalize().into();
    let mut h = Sha256::new();
    h.update(stage1);
    h.finalize().into()
}

pub fn verify(challenge: &[u8], client_token: &[u8], stored_stage2: &Stage2Hash) -> bool {
    if client_token.len() != 20 {
        return false;
    }

    let mut h = Sha1::new();
    h.update(challenge);
    h.update(stored_stage2);
    let xor: [u8; 20] = h.finalize().into();

    let mut stage1 = [0u8; 20];
    for i in 0..20 {
        stage1[i] = client_token[i] ^ xor[i];
    }

    let mut h = Sha1::new();
    h.update(stage1);
    let computed: [u8; 20] = h.finalize().into();
    constant_time_eq_20(&computed, stored_stage2)
}

pub fn verify_sha256(challenge: &[u8], client_token: &[u8], stored_stage2: &Sha256Hash) -> bool {
    if client_token.len() != 32 {
        return false;
    }

    let mut h = Sha256::new();
    h.update(challenge);
    h.update(stored_stage2);
    let xor: [u8; 32] = h.finalize().into();

    let mut stage1 = [0u8; 32];
    for i in 0..32 {
        stage1[i] = client_token[i] ^ xor[i];
    }

    let mut h = Sha256::new();
    h.update(stage1);
    let computed: [u8; 32] = h.finalize().into();
    constant_time_eq_32(&computed, stored_stage2)
}

fn constant_time_eq_20(a: &[u8; 20], b: &[u8; 20]) -> bool {
    let mut diff = 0u8;
    for i in 0..20 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Authentication configuration for the proxy.
/// In pass-through mode, the proxy relays the challenge to the real MySQL.
/// In verify mode, the proxy checks credentials itself before forwarding.
#[derive(Clone, Default)]
pub struct AuthConfig {
    pub user: Option<String>,
    pub stage2: Option<Stage2Hash>,
    pub sha256_stage2: Option<Sha256Hash>,
}

impl AuthConfig {
    pub fn from_credentials(user: &str, password: &str) -> Self {
        Self {
            user: Some(user.to_string()),
            stage2: Some(stage2_hash(password)),
            sha256_stage2: Some(sha256_stage2(password)),
        }
    }

    pub fn is_open(&self) -> bool {
        self.user.is_none()
    }

    pub fn check(&self, challenge: &[u8], username: &str, token: &[u8]) -> bool {
        if self.is_open() {
            return true;
        }
        let expected_user = self.user.as_deref().unwrap_or("");
        if username != expected_user {
            return false;
        }
        if let Some(stored) = &self.stage2 {
            if token.is_empty() {
                return stored == &stage2_hash("");
            }
            verify(challenge, token, stored)
                || self
                    .sha256_stage2
                    .as_ref()
                    .map_or(false, |s| verify_sha256(challenge, token, s))
        } else {
            true
        }
    }

    pub fn check_sha256(&self, challenge: &[u8], username: &str, token: &[u8]) -> bool {
        if self.is_open() {
            return true;
        }
        if username != self.user.as_deref().unwrap_or("") {
            return false;
        }
        self.sha256_stage2
            .as_ref()
            .map_or(false, |s| verify_sha256(challenge, token, s))
    }
}
