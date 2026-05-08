//! Query fingerprinting — normalizes SQL queries by replacing literal values with `?`.
//! Used for analytics grouping and cache key generation.
#![allow(unused)]

/// Normalize a SQL query by replacing literal values with placeholders.
/// Returns the fingerprint string.
pub fn fingerprint(sql: &str) -> String {
    let mut result = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        match bytes[i] {
            // Single-quoted string literals
            b'\'' => {
                result.push('?');
                i += 1;
                while i < len {
                    if bytes[i] == b'\\' {
                        i += 2; // skip escaped char
                    } else if bytes[i] == b'\'' {
                        if i + 1 < len && bytes[i + 1] == b'\'' {
                            i += 2; // escaped quote ''
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            // Double-quoted tokens:
            // - PostgreSQL commonly uses them for identifiers ("Table"."column").
            // - Some MySQL modes use them for string literals.
            // Heuristic: keep as-is when token has no whitespace; otherwise
            // treat as a string literal and replace with '?'.
            b'"' => {
                let start = i;
                i += 1;
                let mut has_whitespace = false;
                while i < len {
                    if bytes[i] == b'"' {
                        if i + 1 < len && bytes[i + 1] == b'"' {
                            i += 2; // escaped quote ""
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    if bytes[i].is_ascii_whitespace() {
                        has_whitespace = true;
                    }
                    i += 1;
                }
                if has_whitespace {
                    result.push('?');
                } else {
                    result.push_str(&sql[start..i]);
                }
            }
            // Numeric literals
            b'0'..=b'9' => {
                // Check if preceded by an identifier char (part of a name like `t1`)
                if !result.is_empty() {
                    let last = result.as_bytes()[result.len() - 1];
                    if last.is_ascii_alphanumeric() || last == b'_' {
                        result.push(bytes[i] as char);
                        i += 1;
                        continue;
                    }
                }
                result.push('?');
                // Skip the full number (digits, dots, hex, scientific notation)
                while i < len
                    && (bytes[i].is_ascii_digit()
                        || bytes[i] == b'.'
                        || bytes[i] == b'x'
                        || bytes[i] == b'X'
                        || bytes[i] == b'e'
                        || bytes[i] == b'E'
                        || bytes[i] == b'+'
                        || bytes[i] == b'-'
                        || (bytes[i] >= b'a' && bytes[i] <= b'f')
                        || (bytes[i] >= b'A' && bytes[i] <= b'F'))
                {
                    i += 1;
                }
            }
            // Collapse whitespace
            b' ' | b'\t' | b'\n' | b'\r' => {
                result.push(' ');
                while i < len && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
            }
            // Everything else passes through
            _ => {
                result.push(bytes[i] as char);
                i += 1;
            }
        }
    }

    result
}

/// Compute a hash of the fingerprint for use as cache/analytics key.
pub fn fingerprint_hash(sql: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let fp = fingerprint(sql);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    fp.hash(&mut hasher);
    hasher.finish()
}

/// Compute both the fingerprint and its hash in a single pass.
pub fn fingerprint_with_hash(sql: &str) -> (String, u64) {
    use std::hash::{Hash, Hasher};
    let fp = fingerprint(sql);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    fp.hash(&mut hasher);
    (fp, hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fingerprint_basic() {
        let sql = "SELECT * FROM users WHERE id = 42 AND status = 'active'";
        let fp = fingerprint(sql);
        assert_eq!(fp, "SELECT * FROM users WHERE id = ? AND status = ?");
    }

    #[test]
    fn test_fingerprint_insert() {
        let sql = "INSERT INTO logs (msg, ts) VALUES ('hello world', 1234567890)";
        let fp = fingerprint(sql);
        assert_eq!(fp, "INSERT INTO logs (msg, ts) VALUES (?, ?)");
    }

    #[test]
    fn test_fingerprint_preserves_table_names() {
        let sql = "SELECT * FROM t1 JOIN t2 ON t1.id = t2.t1_id";
        let fp = fingerprint(sql);
        assert_eq!(fp, "SELECT * FROM t1 JOIN t2 ON t1.id = t2.t1_id");
    }
}
