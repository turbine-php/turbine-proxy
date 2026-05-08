#!/bin/bash
# Runs inside the postgres16 container on first start (via initdb hook).
# Creates the replication user and the test database schema.
set -e

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<-EOSQL
    -- Allow replica to connect for streaming replication
    CREATE ROLE replicator WITH REPLICATION LOGIN PASSWORD 'replicator';

    -- Grant the replication user access from any host (pg_hba via trust mode handles auth)
    GRANT CONNECT ON DATABASE "$POSTGRES_DB" TO replicator;

    -- Base schema used by integration tests
    CREATE TABLE IF NOT EXISTS it_basic (
        id   SERIAL PRIMARY KEY,
        val  TEXT
    );
    CREATE TABLE IF NOT EXISTS it_types (
        id      SERIAL PRIMARY KEY,
        i_col   INTEGER,
        f_col   DOUBLE PRECISION,
        t_col   TEXT,
        b_col   BOOLEAN,
        ts_col  TIMESTAMPTZ DEFAULT now()
    );
    CREATE TABLE IF NOT EXISTS it_txn (
        id  INTEGER PRIMARY KEY,
        val INTEGER
    );
    CREATE TABLE IF NOT EXISTS it_large (
        id  SERIAL PRIMARY KEY,
        pad TEXT
    );
EOSQL

# Update pg_hba.conf to allow replication from any local address
echo "host replication replicator 0.0.0.0/0 trust" >> "$PGDATA/pg_hba.conf"
