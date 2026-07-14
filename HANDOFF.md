# Termite — Conversation Handoff (v7)

This document gives the next conversation full context to continue development without losing anything. It supersedes v6. **Read this one — CI is now actually green on GitHub (not just locally); M3's protocol layer remains complete and is the last milestone-level state that changed.**

---

## What happened since v6 (2026-07-14)

v6 claimed `12b8518` ("fix(ci): unbreak audit/deny/Linux-test jobs") fixed CI but had never actually been watched against a real run — it hadn't. It had in fact already been pushed (v6 was wrong that push was still outstanding) and the real run showed only the `Security Audit` job failing, for two independent reasons fixed in two follow-up commits:

1. **`RUSTSEC-2023-0071` not ignored in CI** (`298b50d`). `rustsec/audit-check@v2` does not read `.cargo/audit.toml` the way the `cargo-audit` CLI does — its ignore list is a separate `ignore:` action input. Without it, the deliberately-unfixed RSA advisory (see `.cargo/audit.toml`/`deny.toml`) failed the job even though `cargo deny` and local `cargo audit` both passed. Fixed by passing `ignore: RUSTSEC-2023-0071,RUSTSEC-2026-0194,RUSTSEC-2026-0195` on the step.
2. **Missing `checks: write` permission** (`49c59ea`). After (1), the underlying audit itself passed clean, but the action still failed with `403 Resource not accessible by integration` when it tried to publish results via the Checks API — this repo's default `GITHUB_TOKEN` is read-only and the workflow had no `permissions:` block at all. Fixed by scoping `contents: read` + `checks: write` to the `audit` job.

Verified against the real run (not just locally): `https://github.com/brass-ape/termite/actions/runs/29308863652` — all six jobs (Lint, Test × 3 OS, Cargo Deny, Security Audit) green on `49c59ea`.

**Lesson for future CI changes to this repo**: GitHub Actions that wrap a CLI tool (audit-check wrapping cargo-audit) don't necessarily inherit that CLI's config-file conventions, and don't necessarily inherit sensible default token permissions either. A commit message claiming something is "verified" for a GitHub Action is not evidence until a real workflow run has been checked via the API or `gh run list/view` (not installed in this environment — used raw `curl` against the public REST API instead, which works unauthenticated for public repos except log downloads, which need admin/token auth).

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
- **Git:** `main`, clean, pushed and up to date with `origin/main` at `49c59ea`. CI is green on GitHub (verified via the Actions API, not just assumed).
- Verify with `git status` / `git show --stat` rather than trusting this document or commit messages blindly — commits `7a39679`/`906634e` are the standing example of why, and `12b8518`'s "unbreak CI" claim (see above) is the same lesson applied to CI status specifically: don't trust "should work" for a GitHub Actions change, check the actual run.
- Environment: Arch, Hyprland/Wayland, Rust 1.97, repo-local git identity, `cargo-audit`/`cargo-deny` binaries in `~/.local/bin`. No `gh` CLI installed — checked Actions status via unauthenticated `curl` against `api.github.com/repos/brass-ape/termite/actions/runs` (works for run/job status on this public repo; job log downloads 403 without admin token).

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

1. **Start M4 (host management UI)** — CI is settled and M3's protocol surface is complete; M4 is the first consumer of all of it: `SessionEvent`/`SessionCommand` wiring into Iced, host profiles from `termite-storage`, `SshConfig` alias resolution, passphrase prompt dialog, key-gen UI. This is now the only pending item.

---

*Handoff v7 written after actually verifying CI goes green on GitHub (two real bugs in the audit-check step, both fixed and confirmed against a live run). v6 had claimed this was done and pushed; neither was true yet. Working agreement: commit actively as work lands (the user asked for this explicitly); push only when asked.*
