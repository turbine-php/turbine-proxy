#!/usr/bin/env bash
# tests/fixtures/chaos_replication_setup.sh
# Wires mysql-primary → mysql-replica1 and mysql-primary → mysql-replica2
# using GTID-based replication. Run once after all three nodes are healthy.
set -euo pipefail

PRIMARY_HOST="${PRIMARY_HOST:-mysql-primary}"
REPLICA1_HOST="${REPLICA1_HOST:-mysql-replica1}"
REPLICA2_HOST="${REPLICA2_HOST:-mysql-replica2}"
PASS="${MYSQL_ROOT_PASSWORD:-root}"

wait_mysql() {
  local host="$1"
  echo "[setup] waiting for MySQL at $host..."
  until mysqladmin ping -h "$host" -uroot -p"$PASS" --silent 2>/dev/null; do
    sleep 2
  done
  echo "[setup] $host ready"
}

wait_mysql "$PRIMARY_HOST"
wait_mysql "$REPLICA1_HOST"
wait_mysql "$REPLICA2_HOST"

# Create replication user on primary
mysql -h "$PRIMARY_HOST" -uroot -p"$PASS" <<SQL
CREATE USER IF NOT EXISTS 'repl'@'%' IDENTIFIED WITH mysql_native_password BY 'replpass';
GRANT REPLICATION SLAVE ON *.* TO 'repl'@'%';
FLUSH PRIVILEGES;
SQL

configure_replica() {
  local host="$1"
  echo "[setup] configuring replica at $host"
  mysql -h "$host" -uroot -p"$PASS" <<SQL
STOP REPLICA;
CHANGE REPLICATION SOURCE TO
  SOURCE_HOST='${PRIMARY_HOST}',
  SOURCE_PORT=3306,
  SOURCE_USER='repl',
  SOURCE_PASSWORD='replpass',
  SOURCE_AUTO_POSITION=1;
START REPLICA;
SQL
  # Wait for replica threads to start
  for i in $(seq 1 20); do
    local status
    status=$(mysql -h "$host" -uroot -p"$PASS" -sNe \
      "SELECT COUNT(*) FROM performance_schema.replication_connection_status WHERE SERVICE_STATE='ON'" 2>/dev/null || echo 0)
    if [ "$status" -gt 0 ]; then
      echo "[setup] $host replication running"
      return
    fi
    sleep 2
  done
  echo "[setup] WARNING: $host replication may not have started"
}

configure_replica "$REPLICA1_HOST"
configure_replica "$REPLICA2_HOST"

echo "[setup] Replication wired: primary → replica1, primary → replica2"
