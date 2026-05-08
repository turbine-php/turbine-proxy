---
sidebar_position: 8
---

# Grafana Integration

TurbineProxy exposes a SimpleJSON datasource endpoint compatible with the [Grafana SimpleJSON plugin](https://grafana.com/grafana/plugins/grafana-simple-json-datasource/).

## Setup

1. Install the SimpleJSON plugin in Grafana
2. Add a new datasource: **SimpleJSON**
3. Set the URL to `http://your-proxy-host:8080/grafana`
4. Save & Test — should show "Data source is working"

## Available Metrics

| Metric | Description |
|---|---|
| `queries_total` | Cumulative query count |
| `queries_read` | Cumulative read query count |
| `queries_write` | Cumulative write query count |
| `connections_active` | Current active connections |
| `connections_total` | Cumulative connection count |
| `queries_per_minute` | Throughput time series |
| `p95_latency_ms` | P95 query latency over time |

## Endpoints

| Endpoint | Description |
|---|---|
| `GET /grafana/` | Health check |
| `POST /grafana/search` | Returns list of available metrics |
| `POST /grafana/query` | Returns time-series data |
| `POST /grafana/annotations` | Returns event annotations |
| `POST /grafana/tag-keys` | Tag key list |
| `POST /grafana/tag-values` | Tag values for a key |

## Prometheus

For Prometheus scraping, use the `/metrics` endpoint instead:

```yaml
# prometheus.yml
scrape_configs:
  - job_name: turbineproxy
    static_configs:
      - targets: ['localhost:8080']
    metrics_path: /metrics
```
