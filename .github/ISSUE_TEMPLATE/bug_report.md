---
name: Bug Report
about: Report a bug or unexpected behavior
labels: bug
---

## Description

<!-- A clear and concise description of the bug. -->

## Steps to Reproduce

1. 
2. 
3. 

## Expected Behavior

<!-- What you expected to happen. -->

## Actual Behavior

<!-- What actually happened. Include error messages and logs. -->

## Environment

- TurbineProxy version: <!-- `turbineproxy --version` -->
- Database: <!-- e.g. MySQL 8.0, MariaDB 11.4, PostgreSQL 16 -->
- OS: <!-- e.g. Ubuntu 22.04, macOS 14 -->
- Deploy method: <!-- binary, Docker, docker-compose -->

## Config (sanitized)

```toml
# Paste relevant sections of your turbineproxy.toml here (remove credentials)
```

## Logs

```
# RUST_LOG=debug turbineproxy --config ... 2>&1 | tail -50
```
