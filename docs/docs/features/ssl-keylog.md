---
sidebar_position: 14
---

# SSL Key Log

TurbineProxy can write TLS session secrets to a file in [NSS Key Log Format](https://firefox-source-docs.mozilla.org/security/nss/legacy/key_log_format/index.html). Tools such as Wireshark and `ssldump` can load this file to decrypt TLS traffic captured with `tcpdump` or a network tap.

> [!WARNING]
> **This feature is for debugging only. Never enable it in production.**
> The key log file contains session secrets that allow complete decryption of captured TLS traffic. Treat it with the same sensitivity as a private key.

## How It Works

Each TLS session negotiation writes one line per secret to the file:

```
CLIENT_HANDSHAKE_TRAFFIC_SECRET a1b2c3d4... e5f6a7b8...
SERVER_HANDSHAKE_TRAFFIC_SECRET a1b2c3d4... 9c8d7e6f...
CLIENT_TRAFFIC_SECRET_0 a1b2c3d4... 1a2b3c4d...
SERVER_TRAFFIC_SECRET_0 a1b2c3d4... 5e6f7a8b...
```

This is the standard NSS Key Log format (also used by Firefox, Chrome, and curl when `SSLKEYLOGFILE` is set). Wireshark reads it via **Edit → Preferences → Protocols → TLS → (Pre)-Master-Secret log filename**.

## Configuration

Key logging is configured independently for the **frontend** (client → proxy) and each **backend** (proxy → database) connection.

### Frontend (client → proxy)

```toml
[frontend_tls]
enabled         = true
cert            = "/etc/turbineproxy/server.crt"
key             = "/etc/turbineproxy/server.key"
ssl_keylog_file = "/tmp/turbineproxy-frontend-keys.log"
```

### Backend (proxy → database)

Set `ssl_keylog_file` per backend — primary and replicas can use different paths:

```toml
[shared.primary]
tls_mode        = "verify-identity"
tls_ca          = "/etc/ssl/certs/rds-ca.pem"
ssl_keylog_file = "/tmp/turbineproxy-backend-primary-keys.log"

[[shared.replicas]]
addr            = "replica-1:3306"
tls_mode        = "verify-identity"
ssl_keylog_file = "/tmp/turbineproxy-backend-replica-keys.log"
```

Leave `ssl_keylog_file` empty (or omit it) to disable key logging — this is the default.

## Wireshark Workflow

```bash
# 1. Start capturing on the database port
tcpdump -i any -w capture.pcap port 3306

# 2. Start TurbineProxy with ssl_keylog_file configured
./turbineproxy --config debug.toml

# 3. Reproduce the issue

# 4. Stop both tcpdump and turbineproxy

# 5. Open Wireshark:
#    Edit → Preferences → Protocols → TLS
#    Set "(Pre)-Master-Secret log filename" to the key log path
#    Open capture.pcap → MySQL packets are now decrypted
```

## Security Considerations

| Risk | Mitigation |
|------|-----------|
| Key log readable by other processes | `chmod 600 /tmp/turbineproxy-*.log` immediately after creation |
| Key log persists after debugging session | Delete the file immediately; consider writing to a `tmpfs` mount |
| Key log grows unboundedly | The file is append-only; truncate or delete and `SIGHUP` the proxy to reopen |
| Secrets captured in backup | Exclude the key log path from backup tools |

**On Linux**, use a `tmpfs` to ensure keys never reach disk:

```bash
mount -t tmpfs tmpfs /mnt/keylog
```

```toml
[frontend_tls]
ssl_keylog_file = "/mnt/keylog/turbineproxy-keys.log"
```

## Scope

Key logging captures secrets for TLS sessions negotiated **after** TurbineProxy starts with the option enabled. Existing sessions already established before the option was set are not captured — restart the proxy and reconnect clients to capture a full session.

Sending `SIGHUP` does **not** rotate or clear the key log. To start a clean log, stop the proxy, delete the file, and restart.
