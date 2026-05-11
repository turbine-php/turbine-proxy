---
sidebar_position: 10
---

# Secrets Encryption

TurbineProxy supports loading sensitive values — passwords, API keys — from environment variables and files rather than hardcoding them in the TOML config. It also supports encrypting secrets on disk with AES-256-GCM so that a stolen config file does not expose credentials.

## Option 1: Environment Variable References

Use the `env:` prefix to pull a value from an environment variable at startup:

```toml
[primary]
addr     = "db-primary:3306"
user     = "proxyuser"
password = "env:DB_PASSWORD"
```

When TurbineProxy starts, it reads `$DB_PASSWORD` from the environment. If the variable is not set, startup fails with a clear error.

This is the recommended approach for container deployments, where secrets are injected as environment variables by Kubernetes secrets, Docker secrets, or a secrets manager.

## Option 2: File References

Use the `file:` prefix to read the secret from a file path:

```toml
[primary]
password = "file:/run/secrets/db_password"
```

The file must contain exactly the secret value (one line, no newline, or TurbineProxy trims it automatically). This works well with Docker secrets and Kubernetes volume mounts.

## Option 3: Encrypted Secrets on Disk

For environments where environment variable injection is not possible and you need to store the config file alongside encrypted credentials, use TurbineProxy's built-in AES-256-GCM encryption.

### Step 1: Generate an Encryption Key

```bash
openssl rand -hex 32
```

This produces a 64-character hex string representing a 32-byte key. Example:

```
a3f1c2e4b5d6a7f8e9c0b1a2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2
```

### Step 2: Set the Environment Variable

```bash
export TURBINEPROXY_SECRET_KEY=a3f1c2e4b5d6a7f8e9c0b1a2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2
```

Add this to your systemd unit file, Docker environment, or Kubernetes secret — not to the TOML file.

### Step 3: Encrypt a Secret

```bash
turbineproxy encrypt "yourpassword"
```

Output:

```
enc:aBcDeFgHiJkLmNoPqRsTuVwXyZ...
```

### Step 4: Use the Encrypted Value in Config

```toml
[primary]
addr     = "db-primary:3306"
user     = "proxyuser"
password = "enc:aBcDeFgHiJkLmNoPqRsTuVwXyZ..."
```

TurbineProxy decrypts the value at startup using `TURBINEPROXY_SECRET_KEY`.

### Encryption Details

| Property | Value |
|---|---|
| Algorithm | AES-256-GCM |
| Key derivation | Raw 32-byte key from 64-char hex string |
| Nonce | 12-byte random, generated fresh on each encryption |
| Format on disk | `enc:<base64url(nonce ‖ ciphertext ‖ tag)>` |
| Auth tag | 16 bytes, appended to ciphertext |

A 12-byte random nonce means the same password encrypted twice produces different ciphertext. This is the correct and secure behavior.

## Compatibility Matrix

| Key set | Config value | Result |
|---|---|---|
| Yes | `enc:...` | Decrypts at startup |
| Yes | Plaintext | Used as-is (no decryption needed) |
| Yes | `env:VAR` | Reads from environment variable |
| No | `enc:...` | **Startup error** — key required to decrypt |
| No | Plaintext | Used as-is |
| No | `env:VAR` | Reads from environment variable |

## Key Rotation

To rotate the encryption key:

1. Generate a new key: `openssl rand -hex 32`
2. Re-encrypt all `enc:` values: `turbineproxy encrypt "<plaintext>" --key <new_key>`
3. Update `TURBINEPROXY_SECRET_KEY` to the new key
4. Update the config file with the new `enc:` values
5. Restart TurbineProxy

TurbineProxy does not support online key rotation — a restart is required.

## What Fields Can Be Encrypted?

Any string value in the config file can use `env:`, `file:`, or `enc:` references:

```toml
[dashboard]
username = "env:DASHBOARD_USER"
password = "file:/run/secrets/dashboard_password"

[primary]
password = "enc:aBcDeFg..."

[[replicas]]
password = "enc:XyZaBcD..."
```

## What's Next?

- [Prometheus and Grafana Integration](./prometheus-grafana)
- [How-To: SQL Injection Protection](./howto-sql-injection-protection)
