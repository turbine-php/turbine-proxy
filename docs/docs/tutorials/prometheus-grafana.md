---
sidebar_position: 11
---

# Prometheus and Grafana Integration

TurbineProxy exposes a Prometheus-compatible `/metrics` endpoint and a SimpleJSON datasource for Grafana. This tutorial shows you how to configure both.

## Prerequisites

- TurbineProxy running with `[dashboard] enabled = true`
- Prometheus (for the scrape path)
- Grafana with the SimpleJSON plugin installed (for the SimpleJSON path)

## Prometheus

### Step 1: Verify the Metrics Endpoint

```bash
curl http://localhost:8080/metrics
```

You should see Prometheus text-format output with TurbineProxy metrics:

```
# HELP turbineproxy_queries_total Total number of queries processed
# TYPE turbineproxy_queries_total counter
turbineproxy_queries_total 14821
# HELP turbineproxy_queries_read Total read queries
turbineproxy_queries_read 11036
...
```

### Step 2: Add a Scrape Job to prometheus.yml

```yaml
scrape_configs:
  - job_name: turbineproxy
    static_configs:
      - targets: ['localhost:8080']
    metrics_path: /metrics
    scrape_interval: 15s
```

### Step 3: Reload Prometheus

```bash
curl -X POST http://localhost:9090/-/reload
```

Or restart Prometheus if `--web.enable-lifecycle` is not enabled.

### Available Prometheus Metrics

| Metric | Type | Description |
|---|---|---|
| `turbineproxy_queries_total` | Counter | Total queries processed |
| `turbineproxy_queries_read` | Counter | Read queries routed to replicas |
| `turbineproxy_queries_write` | Counter | Write queries routed to primary |
| `turbineproxy_connections_active` | Gauge | Current connected clients |
| `turbineproxy_connections_total` | Counter | Cumulative connection count |
| `turbineproxy_queries_per_minute` | Gauge | Throughput in queries/minute |
| `turbineproxy_p95_latency_ms` | Gauge | P95 query latency in ms |

## Grafana via SimpleJSON

TurbineProxy also implements the Grafana SimpleJSON protocol at `/grafana`, which lets you visualize metrics in Grafana without Prometheus.

### Step 1: Install the SimpleJSON Plugin

In your Grafana instance:

```bash
grafana-cli plugins install grafana-simple-json-datasource
```

Or install it from the Grafana UI under **Configuration → Plugins → Browse**.

Restart Grafana after installation.

### Step 2: Add the Datasource

1. In Grafana, go to **Configuration → Data Sources → Add data source**
2. Search for and select **SimpleJSON**
3. Set the **URL** to `http://your-proxy-host:8080/grafana`
4. Click **Save & Test** — you should see "Data source is working"

### Step 3: Create a Dashboard

In a new Grafana panel:

1. Select your SimpleJSON datasource
2. Click the metric dropdown — you'll see the available metrics listed
3. Select a metric (e.g., `queries_per_minute`) and click **Run query**

### Available SimpleJSON Metrics

| Metric name | Description |
|---|---|
| `queries_total` | Cumulative query count |
| `queries_read` | Cumulative read query count |
| `queries_write` | Cumulative write query count |
| `connections_active` | Current active connections |
| `connections_total` | Cumulative connection count |
| `queries_per_minute` | Throughput time series |
| `p95_latency_ms` | P95 query latency over time |

### SimpleJSON API Endpoints (Reference)

| Endpoint | Description |
|---|---|
| `GET /grafana/` | Health check |
| `POST /grafana/search` | Returns list of available metrics |
| `POST /grafana/query` | Returns time-series data |
| `POST /grafana/annotations` | Returns event annotations |
| `POST /grafana/tag-keys` | Tag key list |
| `POST /grafana/tag-values` | Tag values for a key |

## Kubernetes: Prometheus Scraping via Pod Annotations

If you are running TurbineProxy on Kubernetes, the Helm chart adds scrape annotations automatically. You can also add them manually:

```yaml
# In your Pod or Deployment spec
metadata:
  annotations:
    prometheus.io/scrape: "true"
    prometheus.io/port: "8080"
    prometheus.io/path: "/metrics"
```

These annotations are picked up by the Prometheus Kubernetes service discovery (when configured with a pod scrape role).

## Pre-Built Grafana Dashboard

TurbineProxy ships a ready-made Grafana dashboard JSON in the `public/grafana/` directory of the docs site. Import it into Grafana:

1. Go to **Dashboards → Import**
2. Upload the JSON file or paste the contents
3. Select your SimpleJSON or Prometheus datasource
4. Click **Import**

The dashboard includes panels for throughput, latency percentiles, connection utilization, read/write split ratio, and slow query trends.

## What's Next?

- [Kubernetes with Helm](./kubernetes-helm)
- [High Availability and Automatic Failover](./high-availability)
