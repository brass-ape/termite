# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Termite — a native, open-source SSH client written entirely in Rust (Linux/macOS/Windows). No accounts, telemetry, subscriptions, or AI features; local-first. Full design rationale, rejected alternatives, and the milestone-by-milestone plan live in `ARCHITECTURE.md` — read it before making any architectural decision, not just this file.

**Read `HANDOFF.md` at the start of a session.** This project uses it as a running log of exact per-milestone progress (what compiles, what's stubbed, what's left) that is more current than the milestone plan in `ARCHITECTURE.md`. Commit messages in this repo are not reliable narration of what a commit actually contains (verify with `git show --stat` rather than trusting the message) — trust `HANDOFF.md` and the code itself over git history.

## Commands

```sh
cargo check --workspace              # fast compile check, no linking
cargo build --workspace              # full build
cargo build --release                # release build (LTO, stripped, codegen-units=1)
cargo test --workspace               # all tests
cargo test -p termite-terminal       # tests for a single crate
cargo test <test_name>               # run a single test by name (across workspace)
cargo clippy --workspace --all-targets -- -D warnings   # lint; CI fails on any warning
cargo fmt                            # format (default rustfmt settings)
cargo fmt --all --check              # check formatting without writing (what CI runs)
cargo audit                          # dependency vulnerability scan
cargo deny check                     # license allow-list + banned deps (see deny.toml; openssl is banned)
```

Linux needs `libxkbcommon-dev` and `pkg-config` system packages for Iced (see `.github/workflows/ci.yml`). CI runs lint (fmt + clippy), test (Linux/macOS/Windows matrix), `cargo audit`, and `cargo deny` on every push/PR to `main`.

The RUSTSEC advisory ignore list is duplicated in three places that must be kept in sync when adding or removing an advisory: `.cargo/audit.toml` (read by the local `cargo audit` CLI), `deny.toml`, and the `ignore:` input on the `rustsec/audit-check` step in `ci.yml` — the GitHub Action does **not** read `.cargo/audit.toml`.

Requires Rust 1.85+ stable (russh 0.62's MSRV).

## Workspace layout

Cargo workspace with one binary (`termite`, just calls `termite_app::run()` from `src/main.rs`) and crates under `crates/`:

- **`termite-core`** — shared types (`SessionId`, `HostId`, `HostProfile`, `AuthMethod`, `ConnectionStatus`, `TermiteError`) and core traits (`KeyProvider`, `CredentialStore`). Zero workspace dependencies — every other crate may depend on it, it depends on nothing internal. Does no I/O.
- **`termite-terminal`** — terminal emulator: VT/ANSI parsing (`vte`), the `TerminalGrid`/`Cell` model, scrollback, PTY management (`portable-pty`). Pure in-memory state; takes bytes in, produces a renderable grid out. Does not render.
- **`termite-ssh`** — SSH protocol layer built on `russh`: connection lifecycle, auth (password/publickey/agent), host key verification, channels, SFTP, port forwarding, ProxyJump. Knows nothing about Iced or the terminal grid; communicates via `tokio::sync::mpsc` (`SessionEvent` out, `SessionCommand` in).
- **`termite-crypto`** — SSH key loading/decryption/generation. All key material in `secrecy::SecretVec<u8>`, zeroed on drop via `zeroize`, never logged. Its `ssh-key` dependency is pinned to the exact version russh re-exports (`=0.7.0-rc.11`) — never bump one without the other.
- **`termite-storage`** — persistent state on disk: host profiles and settings as TOML under the platform config dir (`~/.config/termite/` on Linux, `~/Library/Application Support/termite/` on macOS, `%APPDATA%\termite\` on Windows), known_hosts, and the `CredentialStore` implementation wrapping the OS keychain (`keyring`).
- **`termite-ui`** — reusable Iced widgets (TerminalView, TabBar, SidebarPanel, CommandPalette, etc.) and the theme/color palette (`termite-ui/src/theme.rs`). Takes data from `termite-app`; has no knowledge of SSH or storage.
- **`termite-app`** — top-level `iced::application` wiring: owns `AppState`, defines the top-level `Message`/`AppMessage` enum, routes messages between the SSH/terminal/storage/UI layers, manages SSH session task lifecycles.

Dependency graph is one-directional and compiler-enforced:

```
termite-app → termite-ui, termite-ssh, termite-terminal, termite-storage, termite-crypto
termite-ui, termite-ssh, termite-terminal, termite-storage, termite-crypto → termite-core
termite-core → (nothing internal)
```

Per `Cargo.toml`, heavy dependencies (`russh`, `vte`, `portable-pty`, `keyring`, `ssh-key`) are added to a crate's own `Cargo.toml` only when the milestone that needs them is implemented — they are not pre-declared workspace-wide.

## Architecture patterns (see `ARCHITECTURE.md` §6-8 for full detail)

- **Message flow**: the Iced event loop is the single source of truth. Each SSH session runs as its own Tokio task and talks to the app layer only through mpsc channels (`SessionCommand` app→session, `SessionEvent` session→app), bridged into Iced via a `Subscription`. Never let a background task mutate `AppState` directly.
- **Terminal emulation is three layered pieces**: a `ByteSource` trait (implemented by both `portable-pty` for local shells and an SSH channel for remote sessions) feeds raw bytes to a `vte::Parser`, which calls back into a `TerminalHandler: vte::Perform` that mutates the `TerminalGrid`. The grid is queried read-only by the UI on each draw.
- **ProxyJump** is modeled as a nested `SshSession` whose byte source is the outer session's channel, not a spawned subprocess.
- **Key material never crosses the SSH layer as raw bytes**: `termite-ssh` receives a `Box<dyn KeyProvider>` and calls `.sign(data)`, never touching private key contents directly.

## Security invariants (do not violate)

1. Secrets (passwords, key material) are always `secrecy::SecretString`/`SecretVec<u8>`, never plain `String`/`Vec<u8>`.
2. All secret-holding types implement `ZeroizeOnDrop`.
3. Host key verification is mandatory and stateful: unknown keys prompt the user (`SessionEvent::HostKeyUnknown`), changed keys produce a prominent warning (`SessionEvent::HostKeyMismatch`) — never a silent accept. There is effectively no `StrictHostKeyChecking=no` mode.
4. Passwords/passphrases live only in the OS keychain via `keyring`, never in TOML config files on disk.
5. Secrets must never reach `tracing` log output — rely on `secrecy`'s redacting `Debug` impl rather than hand-rolling redaction.
6. `openssl` is banned as a dependency (enforced by `deny.toml`); SSH/crypto/PTY stacks are pure-Rust (`russh`, `ssh-key`, `vte`, `portable-pty`).

## Code standards (from `CONTRIBUTING.md`)

- No `unwrap()`/`expect()` in library code — return `Result`.
- No `unsafe` without a doc comment justifying why it's sound.
- All public items need doc comments.
- Conventional Commits format: `<type>(<scope>): <description>`, types `feat|fix|refactor|docs|test|chore|perf|security`, scopes matching crate names (`core|ssh|terminal|storage|crypto|ui|app`).
- Changes touching `termite-crypto` or SSH auth paths / credential storage require extra scrutiny and an explicit note on security considerations in the PR.
