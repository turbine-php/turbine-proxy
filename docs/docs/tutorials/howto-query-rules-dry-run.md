---
sidebar_position: 14
---

# How-To: Test Query Rules Safely with Dry Run

Before enabling a query rule in production, use `dry_run = true` to observe how many queries it would match — without changing any routing or blocking behavior. This how-to shows you the complete dry-run workflow.

## What Dry Run Does

A dry-run rule:

- **Logs** every matching query (visible in dashboard and logs)
- **Does not** change routing, apply rate limits, block, or rewrite any query
- **Does not** affect application behavior at all

## Step 1: Write the Rule with dry_run

Add your rule to `turbineproxy.toml` with `dry_run = true`:

```toml
[[query_rules]]
match_digest = "SELECT .* FROM orders WHERE status = \\?"
destination  = "replica"
dry_run      = true
comment      = "DRY RUN: testing orders read routing to replica"
```

## Step 2: Apply the Rule

```bash
curl -X POST http://localhost:8080/api/reload
```

Or send `SIGHUP`:

```bash
kill -HUP $(pgrep turbineproxy)
```

## Step 3: Observe Matches

### Via Dashboard

Open the dashboard at `http://localhost:8080`. The **Config** tab shows all active rules and their dry-run status. The **Queries** tab shows which fingerprints are being matched.

### Via API

```bash
# Get current query stats including dry-run hit counts
curl http://localhost:8080/api/queries | jq '.[] | select(.digest | test("orders"))'
```

### Via Logs

With `RUST_LOG=info`, TurbineProxy logs each dry-run match:

```
INFO turbineproxy::proxy::classifier: [DRY RUN] Rule "DRY RUN: testing orders..." matched query: SELECT * FROM orders WHERE status = ?
```

Run with more verbosity to see matched queries in real time:

```bash
RUST_LOG=debug ./turbineproxy turbineproxy.toml 2>&1 | grep "DRY RUN"
```

## Step 4: Validate the Rule

After observing dry-run matches for a reasonable time (a few minutes of real application traffic), verify:

1. **Expected queries are matched**: The fingerprints shown in the dashboard match the queries you intended to affect
2. **Unexpected queries are not matched**: No false positives — queries you don't want affected are not shown as dry-run matches
3. **Volume is as expected**: The hit count is proportional to what you'd expect from your application's query patterns

## Step 5: Enable the Rule for Real

Once satisfied, remove `dry_run = true`:

```toml
[[query_rules]]
match_digest = "SELECT .* FROM orders WHERE status = \\?"
destination  = "replica"
comment      = "Orders reads go to replica"
```

Reload:

```bash
curl -X POST http://localhost:8080/api/reload
```

The rule is now active.

## Dry Run with Rate Limiting

The same technique works for rate limits. Test first to see which queries would be throttled:

```toml
[[query_rules]]
match_digest = "SELECT .* FROM products"
qps_limit    = 200
dry_run      = true
comment      = "DRY RUN: would we reject any product queries at 200 QPS?"
```

If the dry-run log shows that peak traffic exceeds 200 QPS, set a higher limit before enabling.

## Dry Run with Block Rules

Test blocking rules before they can cause production errors:

```toml
[[query_rules]]
match_pattern = "(?i)SELECT.*FROM.*information_schema"
block         = true
dry_run       = true
comment       = "DRY RUN: how many information_schema queries does the app make?"
```

If the app makes legitimate `information_schema` queries (e.g., ORM schema introspection at startup), you'll see them here — and you can refine the pattern to avoid blocking them before enabling.

## Removing the Rule After Testing

If the dry-run shows the rule would cause problems, simply delete it from the config and reload. No production impact has occurred.

## What's Next?

- [Query Routing and Rate Limiting](./rate-limiting-and-routing)
- [Query Rewriting](./query-rewriting)
- [How-To: Hot Reload Configuration](./howto-hot-reload)
