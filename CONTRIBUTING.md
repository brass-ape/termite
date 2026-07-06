# Contributing to Termite

Thank you for your interest in contributing. This document covers the development workflow, code standards, and review process.

## Before You Start

- Read [ARCHITECTURE.md](ARCHITECTURE.md) to understand the project structure and design decisions.
- Check the issue tracker for existing discussion before opening a new issue.
- For significant changes, open an issue first to discuss the approach — this avoids wasted effort.

## Development Setup

Requirements:
- Rust 1.78+ (stable channel)
- `cargo-audit` — `cargo install cargo-audit`
- `cargo-deny` — `cargo install cargo-deny`

```sh
git clone https://github.com/YOUR_USERNAME/termite
cd termite
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo audit
```

## Commit Style

We follow the [Conventional Commits](https://www.conventionalcommits.org/) specification.

```
<type>(<scope>): <short description>

[optional body]

[optional footer]
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `security`

Scopes map to crate names: `core`, `ssh`, `terminal`, `storage`, `crypto`, `ui`, `app`

Examples:
```
feat(ssh): add Ed25519 public key authentication
fix(terminal): handle OSC sequences with empty parameters
security(crypto): zeroize key material on parse failure
docs(arch): update inter-module communication diagram
```

## Code Standards

- Idiomatic Rust. Prefer clarity over cleverness.
- No `unwrap()` or `expect()` in library code — return `Result`.
- No `unsafe` without a documented safety comment explaining why it is sound.
- All public items must have doc comments.
- No secrets in `Debug` output. Use `secrecy::Secret<T>` for sensitive values.
- Run `cargo clippy --workspace -- -D warnings` before opening a PR. No new warnings.
- Format with `cargo fmt` (default settings).

## Pull Requests

- One logical change per PR.
- PRs must pass CI (clippy, test, audit, build matrix).
- New features should include tests.
- Security-sensitive changes (anything in `termite-crypto`, `termite-ssh` auth paths, credential storage) require extra scrutiny and a note explaining the security considerations.

## Security Issues

Please do not open public issues for security vulnerabilities. See [SECURITY.md](SECURITY.md).
