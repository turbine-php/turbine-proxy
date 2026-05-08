---
sidebar_position: 12
---

# Wire Compression

TurbineProxy supports MySQL wire-protocol compression on backend connections (proxy → database). This reduces bandwidth and can significantly improve throughput on high-latency or metered WAN links.

Compression is negotiated at connection time and is transparent to the client — the client-facing side is never compressed unless the client requests it independently via the MySQL driver.

## Algorithms

| Algorithm | Keyword | MySQL version | Ratio vs uncompressed |
|-----------|---------|---------------|-----------------------|
| None | `"off"` | any | 1× (default) |
| zlib (Deflate) | `"zlib"` | 5.7+ | ~2–4× |
| zstd | `"zstd"` | 8.0.18+ / MariaDB 10.8+ | ~3–6× |

**zstd** is recommended for new deployments. It has lower CPU overhead than zlib at comparable or better compression ratios, and it supports configurable compression levels (the proxy uses the default level 3).

## Configuration

Set `compression` per backend — primary and replicas can have different values:

```toml
[shared.primary]
addr        = "db-primary:3306"
compression = "off"        # local / low-latency — no compression needed

[[shared.replicas]]
addr        = "wan-replica-1:3306"
compression = "zstd"       # WAN replica — compress

[[shared.replicas]]
addr        = "local-replica:3306"
compression = "zlib"       # MySQL 5.7 replica — use compatible algorithm
```

## When to Enable

Enable compression when:

- The proxy and database are in **different data centres** or regions
- Your infrastructure charges for **egress bandwidth**
- The backend connection carries **large result sets** (reporting queries, exports)
- CPU is not the bottleneck (compression is CPU-bound on the proxy)

Leave compression off when:

- Proxy and database are on the same host or LAN (compression adds CPU with no bandwidth benefit)
- Using `fast_forward = true` (the overhead reduction from fast-forward already dominates)

## Negotiation

The proxy sends a `CLIENT_COMPRESS` or `CLIENT_ZSTD_COMPRESSION_ALGORITHM` capability flag during the MySQL handshake. If the server does not support the requested algorithm, the connection falls back to `off` silently.

You can verify negotiated compression in the dashboard **Backends** panel — the `compression` column shows the active algorithm for each backend connection.

## Compatibility Notes

- **zlib** is supported by MySQL 5.7+, MariaDB 5.5+, Amazon Aurora (all versions), and Google Cloud SQL.
- **zstd** requires MySQL 8.0.18+ or MariaDB 10.8+. Amazon Aurora MySQL 3.x (MySQL 8.0-compatible) supports it; Aurora MySQL 2.x does not.
- **MariaDB 10.6+** supports both algorithms.
