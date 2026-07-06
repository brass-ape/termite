# Security Policy

## Supported Versions

Until a stable release exists, only the latest commit on `main` is supported.

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Instead, report them privately:

- **Email:** brass-ape@proton.me
- **Response time:** We aim to acknowledge reports within 72 hours (Or from whenever I check my emails).
- **Disclosure:** We follow a 90-day coordinated disclosure window.

When reporting, please include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Any suggested mitigations

## Security Design Principles

- Secrets (passwords, key material) are wrapped in `secrecy::Secret<T>` and zeroed on drop via `zeroize`.
- Passwords and key passphrases are stored in the OS keychain, never in config files.
- Host key verification is mandatory. Changed host keys produce a prominent warning, never a silent accept.
- No telemetry, no network connections except user-initiated SSH sessions.
- Dependencies are audited via `cargo-audit` in CI.
- Licenses are enforced via `cargo-deny`.
