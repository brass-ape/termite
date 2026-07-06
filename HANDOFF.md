# Termite — Conversation Handoff (v4)

This document gives the next conversation full context to continue development without losing anything. It supersedes v3. **Read this one — it records M1 (done, v3) and M2 (done, this session) as actually, verifiably done.**

---

## M2 is done — `termite-ssh` crate, verified with a real hermetic integration test

Scope for this session, confirmed with the user up front: **`termite-ssh` crate only**, no `termite-app` wiring. There's no host-management UI yet (that's M4) to drive a real connect from, so bridging into an Iced `Subscription` now would just be untested plumbing with no caller — better to build it when M4 gives it a real UI to attach to.

The `russh 0.62.2` API used below was verified by downloading and reading its actual source (client, server, and `keys::known_hosts` modules) before writing any code against it — not guessed from memory or docs.

There's no `sshd` available in this environment (checked: not installed, not running), and installing a system SSH daemon is out of scope for verifying a library crate. So instead of a manual `cargo run` (how M1 was verified), this session wrote a **hermetic end-to-end integration test** (`crates/termite-ssh/tests/session.rs`) that spins up a real, minimal SSH server in-process via `russh::server` on an ephemeral loopback port (`127.0.0.1:0`, freshly generated ed25519 host key) and drives a full, real connection through it — not mocked:

1. Connect → server presents its host key → classified `Unknown` → `SessionEvent::HostKeyUnknown` fires → test sends `SessionCommand::ApproveHostKey(true)`.
2. `SessionEvent::AuthRequired(AuthChallenge::Password)` fires → test responds with `SessionCommand::AuthResponse(AuthResponse::Password(..))` → real password auth round-trip against the in-process server.
3. `SessionEvent::Connected` fires → PTY + shell requested and confirmed (`ChannelMsg::Success` for both).
4. Test sends `SessionCommand::Write(b"hello termite\n")` → server's `data` handler echoes it back over the real channel → `SessionEvent::Output` arrives with **byte-exact** matching content.
5. `SessionCommand::Disconnect` → clean teardown, `SessionEvent::Disconnected { reason: Requested }`.
6. **A second connection, same `known_hosts` file** → host key now classifies `Trusted` → goes straight to `AuthRequired` with no `HostKeyUnknown` prompt — proving the learn/persist round-trip actually works, not just that the code compiles.

This actually ran and passed:
```
running 1 test
test full_session_lifecycle_with_persisted_host_key ... ok
```
Plus 4 unit tests for `known_hosts.rs`'s classification logic (`Unknown`/`Trusted`/`Changed`/replace-then-`Trusted`), all passing. `cargo check --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt -p termite-ssh` (then `--all --check`) are all clean for everything touched this session.

**Not run this session:** `cargo audit` / `cargo deny check` — neither tool is installed in this environment (`which cargo-deny cargo-audit` → not found). `russh` pulls in a large crypto dependency tree (rsa, p256/p384/p521, aes-gcm, argon2, etc.) that hasn't been checked against `deny.toml`'s license allow-list or the vulnerability database. Worth running both before this ships, since CI gates on them.

---

## What this project is

**Termite** — a modern, open-source, native SSH client built entirely in Rust.
- No accounts, no telemetry, no subscriptions, no AI, no cloud.
- Target aesthetic: VS Code / Warp / Obsidian — not PuTTY.
- Cross-platform: Linux, macOS, Windows.
- Philosophy: "It should disappear and let you work."

Full design rationale is in `ARCHITECTURE.md`. Read that first. Also read `CLAUDE.md` — it has the command reference and security invariants and is kept current.

---

## Repo state

- **Location:** `~/Documents/code/termite`
- **Git:** `main` branch. As of session start, `main` was clean and up to date with `origin/main` at `f490a1f`. **This session's changes are uncommitted** — working tree has modifications to `Cargo.toml`/`Cargo.lock`/`crates/termite-ssh/Cargo.toml`/`crates/termite-ssh/src/lib.rs` plus new files (`error.rs`, `events.rs`, `handler.rs`, `known_hosts.rs`, `session.rs`, `tests/session.rs`). Nothing was committed — commit only if/when asked, per this repo's working agreement.
- Verify current state with `git status` / `git show --stat` rather than trusting this document or commit messages blindly — that habit from v3 still applies.

---

## Workspace structure

```
termite/
├── Cargo.toml                  # Workspace + binary package (+ russh, dirs added this session)
├── src/main.rs                 # Entry: calls termite_app::run()
├── ARCHITECTURE.md             # Full design doc — read this
├── CLAUDE.md                   # Command reference, security invariants, kept current
├── CONTRIBUTING.md
├── SECURITY.md
├── LICENSE (MIT)
├── README.md
├── .gitignore / .claudeignore
├── deny.toml                   # cargo-deny: licence allow-list, openssl banned (NOT re-verified against the new russh dep tree this session — see above)
├── .github/workflows/ci.yml    # lint + test (Linux/macOS/Win) + audit + deny
└── crates/
    ├── termite-core/           # Shared types: SessionId, HostProfile, AuthMethod,
    │                           # ConnectionStatus, TermiteError. NO other workspace deps.
    ├── termite-ssh/            # DONE (M2) — see below
    ├── termite-terminal/       # DONE (M1)
    ├── termite-storage/        # STUB — implemented in M3
    ├── termite-crypto/         # STUB — implemented in M3
    ├── termite-ui/             # Theme + colour palette. TermiteTheme type.
    └── termite-app/            # Iced 0.13 app. M1 terminal wired in. NOT touched this session.
```

---

## M2 (SSH core) — final state

### `crates/termite-ssh/src/`
- **`error.rs`** — `SshError` (`thiserror`): wraps `russh::Error` and `russh::keys::Error` via `#[from]`, plus `HostKeyRejected`, `AuthenticationFailed`, `UnsupportedAuthMethod`, `ChannelClosed`, `Io`. `impl From<SshError> for termite_core::TermiteError` maps to `TermiteError::Ssh(String)` at the crate boundary, per the convention already in `termite-core/src/error.rs`.
- **`known_hosts.rs`** — wraps `russh::keys::known_hosts` (which already implements the real OpenSSH `known_hosts` format, including hashed hostnames — not reimplemented here). `known_hosts_path()` resolves `<platform config dir>/termite/known_hosts` via the new `dirs` crate. `HostKeyDecision` (`Trusted`/`Unknown`/`Changed { line }`) comes from classifying `check_known_hosts_path`'s `Ok(true)`/`Ok(false)`/`Err(KeyChanged{line})`. `record()` appends a new trusted entry (wraps `learn_known_hosts_path`); `replace()` handles the mismatch case by deleting the specific stale line then re-recording — only ever called after explicit user approval. **Quirk found and documented in a test comment:** `russh`'s `learn_known_hosts_path` writes a leading blank line when creating a brand-new file (its "does this file already end in a newline" check fails open on an empty file), so the first recorded entry ends up on line 2, not line 1 — don't assume line numbers, always use the value `classify()` actually returns.
- **`events.rs`** — `SessionEvent` (`Connected`, `AuthRequired`, `Output`, `Disconnected`, `Error`, `HostKeyUnknown`, `HostKeyMismatch`), `SessionCommand` (`Write`, `Resize`, `AuthResponse`, `ApproveHostKey`, `Disconnect`), `AuthChallenge`/`AuthResponse` (password only — no `PublicKey`/`KeyboardInteractive` variants added speculatively; M3 adds those when `termite-crypto` exists to back them), `HostKey { algorithm, fingerprint }` (built from `ssh_key::PublicKey::fingerprint(HashAlg::Sha256)` — human-displayable, never raw key bytes), `DisconnectReason`.
- **`handler.rs`** — `SessionHandler` implements `russh::client::Handler`. `type Error = SshError` directly (no wrapper needed — `SshError` already satisfies `From<russh::Error> + Send + Debug` via its `#[from]` variant). `check_server_key` is the security-critical bit: classifies against `known_hosts`, and for `Trusted` returns `Ok(true)` immediately; for `Unknown`/`Changed` it stashes a `oneshot::Sender<bool>` in a shared `Arc<Mutex<Option<..>>>`, fires the matching `SessionEvent`, and `.await`s the oneshot — resolved later by the session task when it sees `SessionCommand::ApproveHostKey`. If the event channel is dead or the oneshot is dropped, it returns `Ok(false)` — **never falls back to accepting**, satisfying the "no silent accept" invariant for real, not just in a doc comment.
- **`session.rs`** — `SshSession::spawn(profile, known_hosts_path, event_tx) -> (SessionId, mpsc::Sender<SessionCommand>)`. Note `known_hosts_path` is a caller-supplied parameter, not resolved internally — this was a deliberate fix mid-session: an earlier draft resolved it internally via `known_hosts::known_hosts_path()`, which would have made every test (and any future caller) silently read/write the real developer's `~/.config/termite/known_hosts`. Spawns a `tokio::task` that: connects (concurrently polling `command_rx` for `ApproveHostKey` via `tokio::select!` against the pinned `connect()` future, since the handshake and the host-key approval have to happen concurrently); on `AuthMethod::Password` fires `AuthRequired` and waits for `AuthResponse`; `AuthMethod::PublicKey`/`Agent` fail immediately and explicitly with "not implemented until M3" rather than silently no-opping; opens a channel, requests PTY + shell and checks for `ChannelMsg::Success` on both (the default `russh::server::Handler::pty_request`/`shell_request` don't reply at all unless a server implementation calls `session.channel_success`, so a client that doesn't check for `Success`/`Failure` would hang forever against a broken/malicious server — checked for real via the test's server, which does reply correctly); then loops `tokio::select!` between `channel.wait()` (→ `Output`/disconnect) and `command_rx.recv()` (→ `Write`/`Resize`/`Disconnect`).
- **`lib.rs`** — module wiring + re-exports (`SshError`, `SessionEvent`, `SessionCommand`, `AuthChallenge`, `AuthResponse`, `HostKey`, `DisconnectReason`, `HostKeyDecision`, `SshSession`).

### `crates/termite-ssh/tests/session.rs`
The hermetic integration test described above. Implements a minimal in-process `russh::server::Handler` (`EchoServer`/`EchoHandler`) accepting a fixed test password, always accepting the session channel, replying `Success` to PTY/shell requests, and echoing `data` bytes back verbatim. Runs on `TcpListener::bind(("127.0.0.1", 0))` so it doesn't need a real port or root/admin privileges, and doesn't touch the real filesystem `known_hosts` (uses a `tempfile::tempdir()`).

### Cargo changes
- Workspace `Cargo.toml`: added `russh = "0.62"` and `dirs = "5"` to `[workspace.dependencies]`.
- `crates/termite-ssh/Cargo.toml`: added `russh`, `tokio`, `tracing`, `dirs`, `secrecy`, `thiserror` (deps) and `tempfile`, `rand = "0.10"` (dev-deps, for the tests' generated keys and temp known_hosts files).

### A real MSRV note
`russh 0.62.2` itself declares `edition = "2024"` / `rust-version = "1.85"` — higher than this workspace's documented `rust-version = "1.78"`. The installed toolchain here is 1.90, and CI (`dtolnay/rust-toolchain@stable`) tracks stable rather than a pinned old version, so nothing actually breaks — but the workspace's `rust-version = "1.78"` field in `Cargo.toml` is now aspirational, not accurate, now that `termite-ssh` depends on `russh`. Not fixed this session (it's a one-line metadata change with no functional effect); worth bumping if anyone relies on that field for real.

Also worth noting for M3: `russh` 0.62.2 pulls in `ssh-key 0.7.0-rc.11` transitively (via `russh::keys`), not the `ssh-key 0.6` version noted in `CLAUDE.md`'s dependency table as the intended pick for `termite-crypto`. When M3 adds its own direct `ssh-key` dependency, it should match `0.7.0-rc.11` (or whatever `russh` pulls in by then) to avoid two incompatible `PublicKey`/`PrivateKey` types existing in the dependency graph simultaneously.

### What M2 does NOT do (deferred, as scoped)
- Any `termite-app`/Iced wiring — no `SessionEvent`/`SessionCommand` flow into `AppMessage`, no `Subscription` bridge. Deliberately deferred until M4's host-management UI gives it a real caller.
- Public-key or SSH-agent authentication — `AuthMethod::PublicKey`/`Agent` fail with an explicit "not implemented until M3" error. Needs `termite-crypto`'s `KeyProvider`.
- Keyboard-interactive auth.
- ProxyJump, port forwarding, SFTP (M7 per the roadmap).
- `cargo audit` / `cargo deny check` against the new dependency tree (tools not installed here).

---

## M1 (terminal emulator) — unchanged this session, still done

See v3 of this document (in git history, or just trust `crates/termite-terminal/` and `crates/termite-app/src/lib.rs` directly — both untouched this session). Summary: PTY spawn (`portable-pty`) + VT parsing (`vte::Parser` + `GridHandler`) + `TerminalGrid`, rendered as monospace text in Iced, with keyboard input mapped back to PTY writes. Verified end-to-end last session via manual `cargo run`.

---

## Key architectural decisions (already locked in)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| UI framework | **Iced 0.13** | Pure Rust, GPU (wgpu), TEA architecture, MIT |
| SSH library | **russh 0.62** | Pure Rust SSH-2, no FFI, MIT |
| VT parsing | **vte 0.13** | Used by Alacritty, Apache-2.0 |
| PTY | **portable-pty 0.8** | Cross-platform, MIT, from WezTerm |
| Async runtime | **tokio 1** | Standard |
| Key formats | **ssh-key** (via `russh::keys`, currently `0.7.0-rc.11`) | RustCrypto, pure Rust — see MSRV/version note above |
| Credential storage | **keyring 2** | OS keychain: macOS/Win/Linux (not yet added — M3) |
| Secret memory | **secrecy 0.10 + zeroize 1** | Mandatory for all secret types |
| Config format | **toml 0.8 + serde 1** | Human-editable |
| Error handling | **thiserror 2** (libs) + **anyhow 1** (app) | Standard |
| Logging | **tracing 0.1** | Structured, async-aware |
| Platform dirs | **dirs 5** (new this session) | `known_hosts` path resolution |

See ARCHITECTURE.md §4 for full crate justifications.

---

## Dependency graph between crates

```
termite-app
  ├── termite-ui      → termite-core, iced
  ├── termite-ssh     → termite-core, russh, tokio, dirs, secrecy
  ├── termite-terminal → termite-core
  ├── termite-storage → termite-core
  └── termite-crypto  → termite-core
termite-core          → (no workspace deps)
```

This is enforced at the compiler level. Nothing leaks across layers.

---

## Security invariants (never break these)

1. Secrets (`passwords`, `key material`) are ALWAYS wrapped in `secrecy::SecretString` or `secrecy::SecretVec<u8>` — never plain `String` or `Vec<u8>`. **Followed in M2**: `AuthResponse::Password` carries a `SecretString`; it's only exposed via `.expose_secret()` at the single point `russh::client::Handle::authenticate_password` needs an owned `String` (an unavoidable boundary crossing — `russh`'s own `auth::Method::Password` field is a plain, non-zeroizing `String` internally, which is a limitation of the upstream crate, not something termite-ssh can fix from outside).
2. `zeroize` zeroes memory on drop. All secret types implement `ZeroizeOnDrop`.
3. Host key verification is MANDATORY. Changed keys produce a prominent warning, never silent accept. **Implemented and tested for real in M2** — see `handler.rs`'s `check_server_key` and the integration test's `HostKeyUnknown` step above.
4. Passwords and key passphrases are stored in the OS keychain (`keyring`), NEVER in config files on disk. (Keyring integration itself is M3; M2's password flow is purely interactive/in-memory via `AuthResponse::Password`, never touches disk.)
5. Secrets NEVER appear in `tracing` log output. `secrecy` enforces this via its `Debug` impl (`[REDACTED]`).

---

## Commit conventions

Conventional Commits format: `<type>(<scope>): <description>`

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `security`
Scopes: `core`, `ssh`, `terminal`, `storage`, `crypto`, `ui`, `app`

Nothing was committed this session (see "Repo state" above) — the working tree has M2's changes uncommitted. A reasonable commit split when asked to commit: one `feat(ssh)` commit for the crate implementation + `Cargo.toml` dependency additions, since the integration test and implementation are tightly coupled and were developed/verified together.

---

## Roadmap — milestone status

| Milestone | Status | Description |
|-----------|--------|-------------|
| **M0** | ✅ Done | Workspace scaffold, CI, window opens |
| **M1** | ✅ Done | Local terminal emulator (PTY + VT emulation + Iced rendering) — verified end-to-end |
| **M2** | ✅ Done | SSH core (password auth, mandatory known_hosts verification) — verified via hermetic in-process integration test this session |
| M3 | Pending | Key auth + credential storage (`termite-crypto`'s `KeyProvider`, `keyring`) |
| M4 | Pending | Host management UI — first real caller for `termite-ssh`'s `SessionEvent`/`SessionCommand` |
| M5 | Pending | Tabs + multi-session |
| M6 | Pending | Advanced terminal (colours, mouse, split, scrollback UI, dynamic resize) |
| M7 | Pending | Port forwarding, SFTP, ProxyJump |
| M8 | Pending | Command palette, UX polish |
| M9 | Pending | Security review, packaging, public release |

---

## Suggested next steps (M3 — key auth + credential storage)

Per `ARCHITECTURE.md` and `CLAUDE.md`, M3 is `termite-crypto` (key loading/decryption/generation via `ssh-key` — pin to `0.7.0-rc.11` to match what `russh` already pulls in, see the version note above) plus `termite-storage`'s `CredentialStore` (OS keychain via `keyring`). Once `KeyProvider` exists, `termite-ssh/src/session.rs`'s `authenticate()` function has two clearly marked stubs (`AuthMethod::PublicKey`/`AuthMethod::Agent` currently return explicit "not implemented until M3" errors) that need real implementations — per `ARCHITECTURE.md` §8, the session should receive a `Box<dyn KeyProvider>` and call `.sign(data)`, never touching raw private key bytes directly (`russh::client::Handle::authenticate_publickey_with` takes a `Signer`, which is the natural fit — worth checking its exact signature against the real `russh` source the same way this session did for the client/server APIs, rather than assuming).

Also opportunistic, not blocking: run `cargo audit` and `cargo deny check` against the new dependency tree (neither was available in this session's environment) before this ships, and consider bumping the workspace's `rust-version` field to reflect what `russh` actually requires.

---

*Handoff v4 written after implementing and verifying M2 (`termite-ssh` core) this conversation. Continue with M3 (key auth + credential storage) next.*
