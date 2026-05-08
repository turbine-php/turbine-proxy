# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| latest  | ✅        |

We patch security issues on the latest release only. We recommend always running the latest version.

## Reporting a Vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Report security issues by emailing **security@turbineproxy.com** with:

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Any suggested fix (optional)

You will receive a response within **48 hours** acknowledging the report.
We aim to release a patch within **14 days** for critical vulnerabilities.

We follow responsible disclosure: we will credit researchers in the release notes
unless you prefer to remain anonymous.

## Scope

In scope:
- Authentication bypass
- SQL injection or data exfiltration via the proxy
- Remote code execution
- Privilege escalation in the dashboard
- Credential exposure (config parsing, logs)

Out of scope:
- Vulnerabilities in the underlying database server
- Issues requiring physical access to the host
- Social engineering
