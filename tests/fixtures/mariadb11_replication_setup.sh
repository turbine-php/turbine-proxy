#!/bin/bash
# Sets up MariaDB GTID replication between primary and replica containers.
# Runs as a one-shot Docker service after both containers are healthy.
set -e

PRIMARY_HOST="${PRIMARY_HOST:-mariadb11}"
REPLICA_HOST="${REPLICA_HOST:-mariadb11-replica}"
ROOT_PASS="${MARIADB_ROOT_PASSWORD:-root}"

echo "[mariadb-repl-setup] Creating replication user on $PRIMARY_HOST ..."
mariadb -h"$PRIMARY_HOST" -uroot -p"$ROOT_PASS" <<'EOF'
CREATE USER IF NOT EXISTS 'replicator'@'%' IDENTIFIED BY 'replicator';
GRANT REPLICATION SLAVE ON *.* TO 'replicator'@'%';
FLUSH PRIVILEGES;
EOF

echo "[mariadb-repl-setup] Pointing replica $REPLICA_HOST → $PRIMARY_HOST ..."
mariadb -h"$REPLICA_HOST" -uroot -p"$ROOT_PASS" <<EOF
STOP REPLICA;
RESET REPLICA ALL;
CHANGE MASTER TO
  MASTER_HOST='$PRIMARY_HOST',
  MASTER_PORT=3306,
  MASTER_USER='replicator',
  MASTER_PASSWORD='replicator',
  MASTER_USE_GTID=current_pos;
START REPLICA;
EOF

echo "[mariadb-repl-setup] Done. Replica status:"
mariadb -h"$REPLICA_HOST" -uroot -p"$ROOT_PASS" \
  -e "SHOW REPLICA STATUS\G" 2>/dev/null \
  | grep -E "Slave_(IO|SQL)_Running|Seconds_Behind_Master" || true
