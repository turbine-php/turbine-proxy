//! Symmetric encryption for secrets stored in the SQLite config database.
//!
//! ## Key configuration
//!
//! Set `TURBINEPROXY_SECRET_KEY` to a 64-character lowercase hex string
//! (32 bytes / 256 bits) before starting the proxy:
//!
//! ```bash
//! # Generate a key:
//! openssl rand -hex 32
//!
//! # Export it:
//! export TURBINEPROXY_SECRET_KEY=<64 hex chars>
//! ```
//!
//! ## On-disk format
//!
//! Encrypted values are stored as `enc:<base64url(nonce || ciphertext)>`
//! where `nonce` is 12 random bytes (AES-256-GCM standard).
//!
//! ## Backward compatibility
//!
//! - Values that do **not** start with `enc:`, `env:`, or `file:` are treated
//!   as plaintext literals (existing configs continue to work).
//! - If `TURBINEPROXY_SECRET_KEY` is not set, new literal passwords are stored
//!   unencrypted and a warning is logged at startup.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    AeadCore, Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

/// Prefix that marks an AES-256-GCM encrypted value in the store.
pub const ENC_PREFIX: &str = "enc:";

/// Read and validate `TURBINEPROXY_SECRET_KEY`.
///
/// Returns `None` if the variable is not set or is invalid (with a log warning).
pub fn load_encryption_key() -> Option<[u8; 32]> {
    let hex = match std::env::var("TURBINEPROXY_SECRET_KEY") {
        Ok(v) => v,
        Err(_) => return None,
    };
    parse_hex_key(&hex)
}

fn parse_hex_key(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        log::warn!(
            "[secret] TURBINEPROXY_SECRET_KEY must be 64 hex chars (got {}); encryption disabled",
            hex.len()
        );
        return None;
    }
    let mut key = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        key[i] = (hi << 4) | lo;
    }
    Some(key)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => {
            log::warn!(
                "[secret] TURBINEPROXY_SECRET_KEY contains non-hex character; encryption disabled"
            );
            None
        }
    }
}

/// Encrypt `plaintext` with AES-256-GCM using `key`.
///
/// Returns a string of the form `enc:<base64url(nonce || ciphertext)>`.
/// Panics if the OS RNG or the cipher fails (should never happen in practice).
pub fn encrypt(plaintext: &str, key: &[u8; 32]) -> String {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .expect("AES-256-GCM encryption failed");
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ciphertext);
    format!("{}{}", ENC_PREFIX, URL_SAFE_NO_PAD.encode(&blob))
}

/// Decrypt a value produced by [`encrypt`].
///
/// Returns `None` if decryption fails (wrong key, corrupted data, etc.).
pub fn decrypt(enc_value: &str, key: &[u8; 32]) -> Option<String> {
    let b64 = enc_value.strip_prefix(ENC_PREFIX)?;
    let blob = URL_SAFE_NO_PAD.decode(b64).ok()?;
    if blob.len() < 12 {
        return None;
    }
    let (nonce_bytes, ciphertext) = blob.split_at(12);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Nonce::from_slice(nonce_bytes);
    let plaintext = cipher.decrypt(nonce, ciphertext).ok()?;
    String::from_utf8(plaintext).ok()
}

/// Prepare a password value for storage.
///
/// - `env:` / `file:` references are stored unchanged (the secret is not on disk).
/// - Literal values are encrypted with `key` when a key is available.
/// - When no key is provided the literal is stored as-is and a warning is logged.
pub fn prepare_for_storage(value: &str, key: Option<&[u8; 32]>) -> String {
    // External references — never encrypt, the actual secret lives elsewhere.
    if value.starts_with("env:") || value.starts_with("file:") || value.starts_with(ENC_PREFIX) {
        return value.to_string();
    }
    // Empty password — no point encrypting.
    if value.is_empty() {
        return value.to_string();
    }
    match key {
        Some(k) => encrypt(value, k),
        None => {
            log::warn!(
                "[secret] TURBINEPROXY_SECRET_KEY not set — storing password as plaintext. \
                 Set the variable to enable at-rest encryption."
            );
            value.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> [u8; 32] {
        [0x42u8; 32]
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = test_key();
        let enc = encrypt("my_secret_password", &key);
        assert!(enc.starts_with(ENC_PREFIX));
        let dec = decrypt(&enc, &key).unwrap();
        assert_eq!(dec, "my_secret_password");
    }

    #[test]
    fn test_encrypt_produces_different_ciphertext_each_time() {
        let key = test_key();
        let a = encrypt("same", &key);
        let b = encrypt("same", &key);
        // Different nonces → different ciphertext
        assert_ne!(a, b);
        assert_eq!(decrypt(&a, &key).unwrap(), "same");
        assert_eq!(decrypt(&b, &key).unwrap(), "same");
    }

    #[test]
    fn test_decrypt_wrong_key_returns_none() {
        let key = test_key();
        let enc = encrypt("secret", &key);
        let wrong_key = [0x99u8; 32];
        assert!(decrypt(&enc, &wrong_key).is_none());
    }

    #[test]
    fn test_decrypt_malformed_returns_none() {
        let key = test_key();
        assert!(decrypt("enc:notbase64!!", &key).is_none());
        assert!(decrypt("enc:", &key).is_none());
        assert!(decrypt("plaintext", &key).is_none());
    }

    #[test]
    fn test_parse_hex_key_valid() {
        let hex = "a".repeat(64);
        let key = parse_hex_key(&hex).unwrap();
        assert_eq!(key, [0xaau8; 32]);
    }

    #[test]
    fn test_parse_hex_key_wrong_length() {
        assert!(parse_hex_key("ab").is_none());
    }

    #[test]
    fn test_prepare_env_reference_unchanged() {
        let key = test_key();
        assert_eq!(prepare_for_storage("env:MY_VAR", Some(&key)), "env:MY_VAR");
    }

    #[test]
    fn test_prepare_file_reference_unchanged() {
        let key = test_key();
        assert_eq!(
            prepare_for_storage("file:/run/secrets/pw", Some(&key)),
            "file:/run/secrets/pw"
        );
    }

    #[test]
    fn test_prepare_already_encrypted_unchanged() {
        let key = test_key();
        let enc = encrypt("val", &key);
        assert_eq!(prepare_for_storage(&enc, Some(&key)), enc);
    }

    #[test]
    fn test_prepare_empty_unchanged() {
        let key = test_key();
        assert_eq!(prepare_for_storage("", Some(&key)), "");
    }

    #[test]
    fn test_prepare_literal_encrypts_when_key_set() {
        let key = test_key();
        let stored = prepare_for_storage("mysecret", Some(&key));
        assert!(stored.starts_with(ENC_PREFIX));
        assert_eq!(decrypt(&stored, &key).unwrap(), "mysecret");
    }

    #[test]
    fn test_prepare_literal_plaintext_when_no_key() {
        let stored = prepare_for_storage("mysecret", None);
        assert_eq!(stored, "mysecret");
    }
}
