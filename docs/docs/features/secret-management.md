---
sidebar_position: 16
---

# Secret Management

TurbineProxy supports three ways to supply passwords for backends and users.
Choose the approach that fits your deployment security requirements.

---

## 1. Literal values (default)

```toml
[[shared.replicas]]
addr     = "replica:3306"
password = "my-db-password"
```

Literal passwords are:
- Never written to log files or the audit log.
- Hashed in memory at startup (SHA-1 and SHA-256 tokens for MySQL auth).
- **Encrypted at rest** in SQLite when `TURBINEPROXY_SECRET_KEY` is set (see below).

---

## 2. External secret references

Prefix a password field value with `env:` or `file:` to have TurbineProxy read
the actual secret at connection time instead of storing it inline.

### `env:` — environment variable

```toml
password = "env:DB_PASSWORD"
```

TurbineProxy calls `std::env::var("DB_PASSWORD")` at runtime. Works well with:
- Docker / Podman `--env-file`
- Kubernetes `envFrom` + `secretKeyRef`
- systemd `EnvironmentFile=`

### `file:` — secret file

```toml
password = "file:/run/secrets/db_pw"
```

TurbineProxy reads the file and trims whitespace. Works well with:
- Docker secret mounts (`/run/secrets/<name>`)
- Vault Agent sinks
- Kubernetes projected volumes

> **Tip:** `env:` and `file:` references are **never** encrypted or re-written —
> the actual secret value never touches the SQLite database.

---

## 3. AES-256-GCM at-rest encryption

Passwords entered or saved through the **dashboard** are stored in SQLite.
Set `TURBINEPROXY_SECRET_KEY` to encrypt them before they are written.

### Threat model

This protects against **offline file theft** — a stolen SQLite backup or disk
image cannot be used to extract credentials without the key.

It does **not** protect against a fully compromised host. If an attacker
controls the running process they can read the decrypted key from memory or
from environment variables. For that level of threat, use a hardware security
module (HSM) or an external secret manager (Vault, AWS Secrets Manager).

### Key sources (priority order)

TurbineProxy tries the following sources in order and uses the first valid key found:

1. **OS keyring** *(requires build flag `--features keyring-support`)*  
   The key is stored in the platform's native secret store — Keychain on macOS,
   libsecret / kwallet on Linux. An attacker who copies the SQLite file or reads
   the filesystem cannot retrieve the key without also having active access to
   the logged-in user session.

   ```bash
   # Store the key in the OS keyring (one-time setup)
   keyring set turbineproxy encryption-key $(openssl rand -hex 32)
   ```

   Build with keyring support:

   ```bash
   cargo build --release --features keyring-support
   ```

2. **`TURBINEPROXY_SECRET_KEY` environment variable**  
   The default and most portable option.

   ```bash
   export TURBINEPROXY_SECRET_KEY=$(openssl rand -hex 32)
   ```

### Generating a key

```bash
# 64 hex characters = 32 bytes = 256-bit key
openssl rand -hex 32
```

Persist the variable in your process manager:

```ini
# systemd unit (EnvironmentFile or Environment=)
TURBINEPROXY_SECRET_KEY=<64-char hex>
```

```yaml
# Docker Compose
environment:
  TURBINEPROXY_SECRET_KEY: "${TURBINEPROXY_SECRET_KEY}"
```

### On-disk format

Encrypted values are stored as:

```
enc:<base64url(12-byte-nonce || ciphertext || 16-byte-auth-tag)>
```

The 12-byte nonce is generated randomly for every write, so two identical
passwords produce different ciphertext values.

### How decryption works at runtime

```
SQLite value            resolve_secret()              auth layer
─────────────────────────────────────────────────────────────────
enc:<base64url>   →   AES-256-GCM decrypt    →   plaintext password
env:MY_VAR        →   std::env::var()        →   plaintext password
file:/run/secret  →   fs::read_to_string()   →   plaintext password
plain-text        →   returned as-is         →   plaintext password
```

### Backward compatibility

| Scenario | Behaviour |
|----------|-----------|
| Key set, new write | Password encrypted before SQLite insert |
| Key set, existing plaintext | Decrypts as-is (pass-through) — no migration needed |
| Key not set, new write | Password stored as plaintext + warning logged |
| Key not set, `enc:` value found | Warning logged, empty string returned (auth fails safely) |
| `env:` / `file:` values | Always stored unchanged regardless of key |

### Key rotation

1. Decrypt all `enc:` values using the old key (read from DB, call `decrypt`).
2. Re-encrypt with the new key.
3. Update the environment variable (or keyring entry).

There is no built-in rotation command yet — a utility script will be provided in a future release.

---

## Choosing the right approach

| Approach | Secrets on disk | Secrets in SQLite | Rotation |
|----------|:--------------:|:-----------------:|:--------:|
| Literal (no key) | TOML file | Plaintext | Manual |
| Literal + `TURBINEPROXY_SECRET_KEY` | TOML file | Encrypted | Key rotation |
| `env:` reference | Never | Never | Env var update |
| `file:` reference | Secret file only | Never | File update |

For production deployments, the recommended setup is:

- Use `env:` or `file:` references in `turbineproxy.toml`.
- Set `TURBINEPROXY_SECRET_KEY` to protect passwords entered via the dashboard.
