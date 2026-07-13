# Termite — Conversation Handoff (v6)

This document gives the next conversation full context to continue development without losing anything. It supersedes v5. **Read this one — it records the protocol-level work of M3 as complete (agent auth, RSA hash handling, `ssh_config` parsing all landed and verified).**

---

## What happened since v5 (2026-07-13, same day — two sessions)

v5 ended with M3's key-auth core verified and three remaining protocol items. All three are now done:

1. **RSA hash-algorithm threading** (`28c4a53`, tests in `0954e49`). The gap v5 documented is closed: `KeyProvider::sign` now takes the negotiated hash algorithm, `LocalKeyProvider` maps it to `rsa-sha2-256`/`rsa-sha2-512` for RSA keys (ed25519 ignores it, correctly), and `KeyProviderSigner::auth_sign` no longer drops its `hash_alg` parameter. `termite-crypto` declares `ssh-key`'s `rsa` feature; unit tests cover the hash selection. Note: `ssh-key` stays pinned to `=0.7.0-rc.11` to match russh's re-export — don't let the two drift.
2. **SSH agent authentication** (`3b05d82`). `AuthMethod::Agent` connects to `$SSH_AUTH_SOCK` via russh's `AgentClient` (new `crates/termite-ssh/src/agent.rs`), tries each identity the agent offers, and authenticates without key material ever entering the process. Hermetic integration test runs an in-process agent server (`tokio-stream`'s `UnixListenerStream` dev-dep) — `agent_auth_signs_via_the_agent` in `tests/session.rs`.
3. **`~/.ssh/config` parsing** (`16271c7`, this session). New `crates/termite-ssh/src/ssh_config.rs`: own parser per `ARCHITECTURE.md` (no crate dep), covering `Host`/`HostName`/`User`/`Port`/`IdentityFile`/`ProxyJump`. OpenSSH resolution semantics: first obtained value wins, `IdentityFile` accumulates across matching blocks, fnmatch-style patterns (`*`/`?`) with `!` negation, case-insensitive. `Match` blocks are inert (their directives can't leak into the preceding `Host` block); unknown directives are skipped; malformed lines are hard errors with line numbers (`SshError::ConfigParse`) — refusing beats silently misreading auth config. `ProxyJump` is kept as a verbatim string until M7. Exported as `SshConfig`/`HostConfig`; `ssh_config::default_path()` gives `~/.ssh/config`. 9 unit tests. **Nothing consumes it yet** — the natural caller is M4's host management (resolve an alias when the user types one).

Verification at v6: `cargo test --workspace` 31 tests green (13 termite-ssh unit incl. ssh_config, 4 SSH integration incl. agent, 9 crypto, keyring against the real Secret Service), clippy `-D warnings` clean, fmt clean.

---

## What this project is

**Termite** — a modern, open-source, native SSH client built entirely in Rust.
- No accounts, no telemetry, no subscriptions, no AI, no cloud.
- Target aesthetic: VS Code / Warp / Obsidian — not PuTTY.
- Cross-platform: Linux, macOS, Windows.
- Philosophy: "It should disappear and let you work."

Full design rationale is in `ARCHITECTURE.md`. Read that first. Also read `CLAUDE.md` — command reference and security invariants, kept current.

---

## Repo state

- **Location:** `~/Documents/code/termite`
- **Git:** `main`, clean. Everything is committed through `16271c7` (ssh_config parser + `SshError::ConfigParse`). **Still never pushed since the deny.toml/audit fixes (`94a2fba`)** — the GitHub actions (`rustsec/audit-check`, `cargo-deny-action`) haven't run against the migrated config; push when the user wants CI to run and watch that first run.
- Verify with `git status` / `git show --stat` rather than trusting this document or commit messages blindly — commits `7a39679`/`906634e` are the standing example of why.
- Environment: Arch, Hyprland/Wayland, Rust 1.97, repo-local git identity, `cargo-audit`/`cargo-deny` binaries in `~/.local/bin`.

---

## Milestone status

| Milestone | Status | Description |
|-----------|--------|-------------|
| **M0** | ✅ Done | Workspace scaffold, CI, window opens |
| **M1** | ✅ Done | Local terminal emulator (PTY + VT + Iced rendering) — verified end-to-end |
| **M2** | ✅ Done | SSH core (password auth, mandatory known_hosts verification) — hermetic integration test |
| **M3** | 🟢 Protocol layer done | Key auth + credential storage. Done: ed25519 generate/load/encrypt/decrypt, RSA loading + correct `rsa-sha2-*` hash selection, passphrase decryption + prompt event flow, `KeyProvider`/`LocalKeyProvider`, publickey auth end-to-end, SSH agent auth, `CredentialStore` on the OS keychain, `~/.ssh/config` parsing. **Remaining are UI-facing items that land with M4+**: passphrase prompt dialog, key-gen UI, optional passphrase/password save toggles. ECDSA loading is not enabled (ssh-key `p256` feature undeclared) — deliberate cut unless a user needs it; ed25519/RSA cover the field |
| M4 | Pending | Host management UI — first real caller for `SessionEvent`/`SessionCommand`, for `SshConfig` alias resolution, and for the M3 UI leftovers |
| M5–M9 | Pending | Tabs, advanced terminal, forwarding/SFTP/ProxyJump, palette, release |

---

## Layout / architecture

Workspace: `termite` binary + `crates/{termite-core,-ssh,-terminal,-storage,-crypto,-ui,-app}`. Dependency graph is one-directional, compiler-enforced, `termite-core` at the bottom with no internal deps. (v4 in git history has the file-by-file M2 walkthrough.)

M3 files, all verified:
- `crates/termite-core/src/traits.rs` — `KeyProvider` (`public_key_blob()`, `sign(data, hash_alg)`), `CredentialStore`.
- `crates/termite-crypto/src/{key,provider,error}.rs` — key generate/load/decrypt/save (key material in `ssh_key::PrivateKey`, zeroizing; passphrases only exposed at the decrypt/encrypt call), `LocalKeyProvider` with per-algorithm signature naming.
- `crates/termite-ssh/src/signer.rs` — `KeyProviderSigner`, the russh `Signer` adapter. Load-bearing contract: `auth_sign` must return the **entire `to_sign` buffer with the signature appended** as a length-prefixed SSH string — russh slices the userauth packet out of it. Returning bare signature bytes makes the session hang (v5 has the full diagnosis).
- `crates/termite-ssh/src/agent.rs` — `$SSH_AUTH_SOCK` agent auth.
- `crates/termite-ssh/src/ssh_config.rs` — the config parser (see above).
- `crates/termite-ssh/src/session.rs` — `authenticate_publickey` (passphrase prompt flow via `AuthRequired`), agent dispatch.
- `crates/termite-storage/src/credential_store.rs` — `KeyringStore` (OS keychain) + `MemoryStore`; integration test hits the real Secret Service.

Security invariants: unchanged from CLAUDE.md, all honored — secrets in `secrecy` types, no silent host-key accepts, keychain-only persistence, no secrets in logs, openssl banned, agent auth never touches key bytes.

---

## Suggested next steps

1. **Push and watch CI** — still outstanding from v5. The migrated `deny.toml` + `.cargo/audit.toml` have never met the real GitHub actions.
2. **Start M4 (host management UI)** — M3's protocol surface is complete and M4 is the first consumer of all of it: `SessionEvent`/`SessionCommand` wiring into Iced, host profiles from `termite-storage`, `SshConfig` alias resolution, passphrase prompt dialog, key-gen UI.

---

*Handoff v6 written after landing the last M3 protocol item (`ssh_config` parsing); RSA hash threading and agent auth landed between v5 and v6. Working agreement: commit actively as work lands (the user asked for this explicitly).*
