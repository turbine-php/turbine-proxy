# Contributing to TurbineProxy

Thank you for your interest in contributing! This document explains how to get started.

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md).
By participating you agree to abide by its terms.

## How to Contribute

### Reporting Bugs

Open a [bug report](.github/ISSUE_TEMPLATE/bug_report.md) and include:
- TurbineProxy version (`turbineproxy --version`)
- Database type and version (MySQL 8.0, MariaDB 11.4, PostgreSQL 16…)
- Minimal config that reproduces the issue
- Relevant log output (`RUST_LOG=debug`)

### Suggesting Features

Open a [feature request](.github/ISSUE_TEMPLATE/feature_request.md) describing the use case
and expected behavior before writing any code.

### Pull Requests

1. **Fork** the repository and create a branch from `main`:
   ```
   git checkout -b feat/my-feature
   ```

2. **Commit messages** must follow [Conventional Commits](https://www.conventionalcommits.org/):
   ```
   feat(proxy): add read/write splitting for PostgreSQL
   fix(analytics): prevent heatmap crash on empty data
   docs(config): add TLS examples
   ```
   Git hooks (`lefthook`) enforce this automatically — run `lefthook install` once after cloning.

3. **Code style**:
   - Rust: `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings`
   - Frontend: `npm run lint` inside `dashboard/`

4. **Tests**: add or update tests for any logic change:
   - Unit tests: `#[cfg(test)]` modules inside the changed file
   - Integration tests: `tests/integration_tests.rs` (MySQL) or `tests/pg_integration_tests.rs`

5. **Open a PR** against `main`. The CI must pass before merging.

## Development Setup

### Prerequisites

- Rust stable (`rustup update stable`)
- Node.js 20+
- Docker (for integration tests)

### Running locally

```bash
# Build backend + frontend (build.rs runs npm automatically)
cargo build

# Run with example config
cargo run -- --config turbineproxy.example.toml

# Unit tests (no database needed)
cargo test --lib

# Integration tests (requires Docker)
docker compose up mysql80 -d
cargo test --test integration_tests -- --test-threads=1

# Frontend dev server
cd dashboard && npm install && npm run dev
```

### Install git hooks

```bash
lefthook install
```

## Project Structure

```
src/             # Rust source — proxy core
  config/        # TOML config parsing
  protocol/      # MySQL & PostgreSQL wire protocol
  proxy/         # Connection pool, classifier, fingerprint
  analytics/     # Query analytics storage (SQLite)
  dashboard/     # Embedded web dashboard (axum)
dashboard/       # React frontend (Vite)
docs/            # Docusaurus documentation site
tests/           # Integration & cluster tests
```

## License

By contributing you agree that your contributions will be licensed under
the same [Apache-2.0](LICENSE-APACHE) terms as the project.
