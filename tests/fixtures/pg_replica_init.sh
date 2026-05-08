#!/bin/bash
# Runs as the entrypoint of the postgres16-replica container.
# Waits for the primary to be ready, then bootstraps a streaming replica.
set -e

PRIMARY_HOST="${PG_PRIMARY_HOST:-postgres16}"
PRIMARY_PORT="${PG_PRIMARY_PORT:-5432}"
PGDATA="${PGDATA:-/var/lib/postgresql/data}"

echo "[replica-init] Waiting for primary $PRIMARY_HOST:$PRIMARY_PORT …"
until pg_isready -h "$PRIMARY_HOST" -p "$PRIMARY_PORT" -U postgres; do
    sleep 1
done

# Only run pg_basebackup if the data directory is empty.
if [ -z "$(ls -A "$PGDATA" 2>/dev/null)" ]; then
    echo "[replica-init] Running pg_basebackup …"
    pg_basebackup \
        -h "$PRIMARY_HOST" \
        -p "$PRIMARY_PORT" \
        -U replicator \
        -D "$PGDATA" \
        -Fp -Xs -P -R
    echo "[replica-init] pg_basebackup done."
else
    echo "[replica-init] Data directory not empty — skipping pg_basebackup."
fi

# Start PostgreSQL
exec docker-entrypoint.sh postgres \
    -c hot_standby=on \
    -c hot_standby_feedback=on \
    -c log_min_messages=warning
