---
sidebar_position: 12
---

# Kubernetes with Helm

TurbineProxy ships an official Helm chart for Kubernetes deployments. This tutorial walks you through installing the chart, configuring it for production, enabling persistence for analytics data, and setting up autoscaling.

## Prerequisites

- Kubernetes cluster (1.21+)
- Helm 3.x installed
- kubectl configured against the target cluster

## Chart Overview

The Helm chart deploys:

- A **Deployment** with configurable replica count (default: 2)
- A **Service** exposing the MySQL proxy port (3307) and the dashboard port (8080)
- A **ConfigMap** holding your `turbineproxy.toml`
- An optional **PersistentVolumeClaim** for analytics data
- An optional **HorizontalPodAutoscaler**

The chart enforces secure defaults: non-root container, read-only root filesystem, all Linux capabilities dropped.

## Step 1: Install the Chart

```bash
helm install turbineproxy ./deploy/helm/turbineproxy \
  --namespace turbineproxy \
  --create-namespace
```

Or, if the chart is published to a Helm repository:

```bash
helm repo add turbineproxy https://your-helm-repo-url
helm install turbineproxy turbineproxy/turbineproxy \
  --namespace turbineproxy \
  --create-namespace
```

## Step 2: Configure the Proxy

All configuration is passed through the `config` value in `values.yaml`. Provide your complete `turbineproxy.toml` as a multi-line string:

```bash
helm install turbineproxy ./deploy/helm/turbineproxy \
  --namespace turbineproxy \
  --create-namespace \
  --set-string 'config=listen_addr = "0.0.0.0:3307"
max_connections = 1000
pool_size = 20

[primary]
addr     = "mysql-primary:3306"
user     = "proxyuser"
password = "env:DB_PASSWORD"
database = "myapp"

[[replicas]]
addr   = "mysql-replica-1:3306"
weight = 100

[analytics]
enabled = true

[dashboard]
enabled     = true
listen_addr = "0.0.0.0:8080"
'
```

For more complex configs, use a `values.yaml` file:

```yaml
# custom-values.yaml
replicaCount: 3

image:
  repository: ghcr.io/turbine-php/turbine-proxy
  tag: "v0.3.6"

config: |
  listen_addr = "0.0.0.0:3307"
  max_connections = 1000
  pool_size = 20

  [primary]
  addr     = "mysql-primary:3306"
  user     = "proxyuser"
  password = "env:DB_PASSWORD"
  database = "myapp"

  [[replicas]]
  addr   = "mysql-replica-1:3306"
  weight = 100

  [ha]
  enabled            = true
  max_replica_lag_ms = 2000

  [analytics]
  enabled = true

  [dashboard]
  enabled     = true
  listen_addr = "0.0.0.0:8080"
```

```bash
helm install turbineproxy ./deploy/helm/turbineproxy \
  -f custom-values.yaml \
  --namespace turbineproxy \
  --create-namespace
```

## Step 3: Handle Secrets

Never put passwords in `values.yaml` or `values.override.yaml` in source control. Use environment variables with the `env:` prefix in your config:

```yaml
config: |
  [primary]
  password = "env:DB_PASSWORD"
```

Then inject `DB_PASSWORD` into pods via a Kubernetes Secret:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: turbineproxy-db-secret
  namespace: turbineproxy
stringData:
  DB_PASSWORD: "yourpassword"
```

Reference the secret in the Deployment by adding an environment variable (via `extraEnv` or by patching the chart):

```yaml
# values.yaml
extraEnv:
  - name: DB_PASSWORD
    valueFrom:
      secretKeyRef:
        name: turbineproxy-db-secret
        key: DB_PASSWORD
```

## Step 4: Enable Persistence for Analytics

By default, analytics data is stored in an `emptyDir` (lost on pod restart). For production, enable a PersistentVolumeClaim:

```yaml
# values.yaml
persistence:
  enabled: true
  storageClass: "standard"   # or your cluster's storage class
  size: 5Gi
  mountPath: /app
```

This creates a PVC bound to the pod. Analytics data (SQLite database) survives pod restarts and upgrades.

## Step 5: Enable Autoscaling

```yaml
autoscaling:
  enabled: true
  minReplicas: 2
  maxReplicas: 10
  targetCPUUtilizationPercentage: 70
```

The HPA scales the Deployment based on CPU utilization. TurbineProxy is stateless at the proxy level (backend pool state is per-pod), so horizontal scaling is safe.

> **Note:** With autoscaling and multiple replicas, each pod has its own analytics store. Aggregate metrics are only visible per-pod. Enable `persistence.enabled = true` to retain analytics between pod restarts.

## Step 6: Verify the Deployment

```bash
kubectl -n turbineproxy get pods
kubectl -n turbineproxy get svc

# Check logs
kubectl -n turbineproxy logs -l app.kubernetes.io/name=turbineproxy --tail=50
```

### Connect from another pod in the cluster

```bash
# Service DNS: <service-name>.<namespace>.svc.cluster.local
mysql -h turbineproxy.turbineproxy.svc.cluster.local -P 3307 -u proxyuser -p myapp
```

## Step 7: Upgrade

After changing `values.yaml`:

```bash
helm upgrade turbineproxy ./deploy/helm/turbineproxy \
  -f custom-values.yaml \
  --namespace turbineproxy
```

The chart computes a `checksum/config` annotation on the pod template — a config change triggers an automatic rolling restart of all pods.

## Health and Readiness Probes

The chart configures both liveness and readiness probes against `/api/health`:

```yaml
livenessProbe:
  httpGet:
    path: /api/health
    port: dashboard   # 8080
  initialDelaySeconds: 10
  periodSeconds: 30

readinessProbe:
  httpGet:
    path: /api/health
    port: dashboard
  initialDelaySeconds: 5
  periodSeconds: 10
```

Pods will not receive traffic until TurbineProxy has successfully connected to the primary backend and passed its first readiness check.

## Prometheus in Kubernetes

The chart adds pod annotations for Prometheus scraping by default:

```yaml
podAnnotations:
  prometheus.io/scrape: "true"
  prometheus.io/port: "8080"
  prometheus.io/path: "/metrics"
```

If you use the Prometheus Operator, create a `ServiceMonitor` instead:

```yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: turbineproxy
  namespace: turbineproxy
spec:
  selector:
    matchLabels:
      app.kubernetes.io/name: turbineproxy
  endpoints:
    - port: dashboard
      path: /metrics
      interval: 15s
```

## What's Next?

- [Prometheus and Grafana Integration](./prometheus-grafana)
- [High Availability and Automatic Failover](./high-availability)
- [Secrets Encryption](./secrets-encryption)
