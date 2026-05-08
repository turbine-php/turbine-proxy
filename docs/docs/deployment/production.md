---
sidebar_position: 1
---

# Production Deployment

## Binary

```bash
cargo build --release
cp target/release/turbineproxy /usr/local/bin/
chmod +x /usr/local/bin/turbineproxy
```

## systemd Service

```ini
# /etc/systemd/system/turbineproxy.service
[Unit]
Description=TurbineProxy MySQL Proxy
After=network.target

[Service]
Type=simple
User=turbineproxy
Group=turbineproxy
ExecStart=/usr/local/bin/turbineproxy /etc/turbineproxy/turbineproxy.toml
Restart=always
RestartSec=5
LimitNOFILE=65536
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable turbineproxy
sudo systemctl start turbineproxy
```

## System User

```bash
sudo useradd --system --no-create-home --shell /bin/false turbineproxy
sudo mkdir -p /etc/turbineproxy /var/lib/turbineproxy /var/log/turbineproxy
sudo chown turbineproxy: /etc/turbineproxy /var/lib/turbineproxy /var/log/turbineproxy
```

## Recommended Config for Production

```toml
listen_addr              = "0.0.0.0:3307"
max_connections          = 2000
pool_size                = 50
connection_max_idle_secs = 55
max_transaction_time_ms  = 60000   # 1 minute hard limit
max_query_time_ms        = 30000   # 30s query limit
read_your_own_writes_ms  = 200
shutdown_timeout_secs    = 30

[analytics]
enabled        = true
db_path        = "/var/lib/turbineproxy/analytics.db"
slow_query_ms  = 100
retention_days = 90

[dashboard]
enabled     = true
listen_addr = "127.0.0.1:8080"   # Bind to localhost, expose via reverse proxy
username    = "admin"
password    = "strongpassword"

[ha]
enabled                    = true
health_check_interval_secs = 5
max_replica_lag_ms         = 2000
primary_failover_threshold = 3
```

## Dashboard Reverse Proxy (nginx)

```nginx
location /proxy/ {
    proxy_pass         http://127.0.0.1:8080/;
    proxy_set_header   Host $host;
    proxy_set_header   X-Real-IP $remote_addr;
    proxy_set_header   X-Forwarded-For $proxy_add_x_forwarded_for;
}
```

## File Descriptor Limits

MySQL proxies can hold many open connections. Ensure the process can open enough file descriptors:

```bash
# /etc/security/limits.d/turbineproxy.conf
turbineproxy soft nofile 65536
turbineproxy hard nofile 65536
```

## Hot Reload

Send `SIGHUP` to reload routing and rewrite rules without restart:

```bash
sudo kill -HUP $(systemctl show --value -p MainPID turbineproxy)
# or via API:
curl -X POST http://localhost:8080/api/reload
```
