#!/bin/bash
# Sets up MySQL GTID replication between primary and replica containers.
# Runs as a one-shot Docker service after both containers are healthy.
set -e

PRIMARY_HOST="${PRIMARY_HOST:-mysql80}"
REPLICA_HOST="${REPLICA_HOST:-mysql80-replica}"
ROOT_PASS="${MYSQL_ROOT_PASSWORD:-root}"

echo "[mysql-repl-setup] Creating replication user on $PRIMARY_HOST ..."
mysql -h"$PRIMARY_HOST" -uroot -p"$ROOT_PASS" <<'EOF'
CREATE USER IF NOT EXISTS 'replicator'@'%' IDENTIFIED BY 'replicator';
GRANT REPLICATION SLAVE ON *.* TO 'replicator'@'%';
FLUSH PRIVILEGES;
EOF

echo "[mysql-repl-setup] Pointing replica $REPLICA_HOST → $PRIMARY_HOST ..."
mysql -h"$REPLICA_HOST" -uroot -p"$ROOT_PASS" <<EOF
STOP REPLICA;
RESET REPLICA ALL;
CHANGE REPLICATION SOURCE TO
  SOURCE_HOST='$PRIMARY_HOST',
  SOURCE_PORT=3306,
  SOURCE_USER='replicator',
  SOURCE_PASSWORD='replicator',
  SOURCE_AUTO_POSITION=1,
  GET_SOURCE_PUBLIC_KEY=1;
START REPLICA;
EOF

echo "[mysql-repl-setup] Done. Replica status:"
mysql -h"$REPLICA_HOST" -uroot -p"$ROOT_PASS" \
  -e "SHOW REPLICA STATUS\G" 2>/dev/null \
  | grep -E "Replica_(IO|SQL)_Running|Seconds_Behind_Source" || true
