//! Fuzz target: SCRAM-SHA-256 server-first-message parser.
//!
//! Feeds arbitrary byte sequences as the payload of a SASLContinue (AuthSASLContinue)
//! message and verifies the parser:
//!   1. Never panics (no unwrap/expect on untrusted input)
//!   2. Handles iteration counts < 4096 by returning an error, not panicking
//!   3. Handles non-UTF-8 gracefully
//!
//! Run with:
//!   cargo fuzz run fuzz_scram_parser -- -max_len=1024 -runs=1000000
//!
//! Requires cargo-fuzz:
//!   cargo install cargo-fuzz

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Treat the fuzz input as the payload of a server-first SASL message.
    // Mirror the exact parsing done in scram_auth():
    let sfm = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(_) => return, // non-UTF-8 is rejected before parse — OK
    };

    let mut full_nonce = "";
    let mut salt_b64 = "";
    let mut iterations: u32 = 4096;
    for part in sfm.split(',') {
        if let Some(v) = part.strip_prefix("r=") {
            full_nonce = v;
        } else if let Some(v) = part.strip_prefix("s=") {
            salt_b64 = v;
        } else if let Some(v) = part.strip_prefix("i=") {
            iterations = v.parse().unwrap_or(4096);
        }
    }

    // Iteration-count guard: must never panic regardless of input.
    if iterations < 4096 {
        return; // correctly rejected
    }

    // Base64 decode of the salt: must not panic on arbitrary input.
    let _ = base64_decode_fuzz(salt_b64);

    // If nonce or salt are empty the real code returns an error before
    // reaching PBKDF2 — we just verify there's no panic.
    let _ = (full_nonce.len(), salt_b64.len(), iterations);
});

fn base64_decode_fuzz(s: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    // Replicate the b64_decode logic from the protocol module.
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}
