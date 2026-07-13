# Termite — Conversation Handoff (v5)

This document gives the next conversation full context to continue development without losing anything. It supersedes v4. **Read this one — it records M1/M2 as done (v3/v4) and the key-auth core of M3 as done and verified (this session).**

---

## What happened this session (2026-07-13)

The previous session died mid-flight when usage ran out; its work was rescued into two raw commits (`7a39679`, `906634e` — the commit messages are noise, the contents are real: `termite-crypto`, `CredentialStore`, `KeyProviderSigner`, publickey auth in `session.rs`, and two then-failing publickey integration tests). This session ran on a **new environment** (the Linux Mint install died; now Arch, Hyprland/Wayland, Rust 1.97) and:

1. **Diagnosed and fixed the hanging publickey integration tests.** Root cause: `KeyProviderSigner::auth_sign` returned only the bare signature blob, but russh's `Signer` contract (verified against russh 0.62.2 source — `auth.rs`, `client/mod.rs`, `client/encrypted.rs`, and `AgentClient::sign_request` as the reference impl) requires returning the **entire `to_sign` buffer with the signature appended** as a length-prefixed SSH string (`u32 len ++ string(alg) ++ string(sig)`). russh slices the userauth packet out of the returned buffer, so returning bare signature bytes made it emit a malformed packet (visible in `RUST_LOG=debug` as a bogus "msg type 123") and hang forever awaiting an auth reply. Fix is confined to `crates/termite-ssh/src/signer.rs` (commit `6af31ce`). All 3 integration tests now pass, including both publickey ones.
2. **Ran full workspace verification**: `cargo check`, `cargo test --workspace` (18 tests, all green — including the keyring tests against the real OS Secret Service), `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all --check`.
3. **Ran `cargo audit` + `cargo deny check` for the first time since russh landed** (the tools were never installed on the old machine; prebuilt binaries now live in `~/.local/bin`). This required real fixes, all in commit `94a2fba`:
   - `deny.toml` used config keys **removed** from current cargo-deny (`[licenses] deny`, `copyleft`) — CI's `cargo-deny-action` would have failed on it. Migrated: the allow-list is the whole policy now.
   - Licenses added to the allow-list after checking which crates carry them: `BSL-1.0` (clipboard-win, error-code — permissive Boost), `MPL-2.0` (option-ext via `dirs` — file-level weak copyleft, used unmodified), `Unicode-3.0` (unicode-ident; replaced the no-longer-present `Unicode-DFS-2016`).
   - Advisory ignores, each with written justification, in **both** `deny.toml` and the new `.cargo/audit.toml` (cargo-audit doesn't read deny.toml; keep them in sync): `RUSTSEC-2023-0071` (rsa Marvin timing sidechannel — no patched release exists, transitive via russh/ssh-key, termite's own key handling is ed25519-only) and `RUSTSEC-2026-0194`/`0195` (quick-xml DoS — build-time `wayland-scanner` proc-macro parsing trusted protocol XML, pinned by winit/smithay).
   - `cargo update -p crossbeam-epoch` (0.9.18 → 0.9.20) cleared `RUSTSEC-2026-0204`.
   - Unmaintained-crate advisories scoped to direct deps (`unmaintained = "workspace"`) — ttf-parser/rustybuzz/instant/paste are pinned by the iced stack and not actionable here.
   - All workspace crates are now `publish = false` (they're internal to the app; also resolves cargo-deny's wildcard-path complaints about version-less `path` deps).
   - Workspace `rust-version` bumped 1.78 → 1.85 (russh's real MSRV); README/CONTRIBUTING/CLAUDE.md updated to match.
4. **Investigated v4's "infinite terminal windows" dev note.** It does **not reproduce** on Arch/Hyprland/Wayland: ran the debug binary from the project root repeatedly — exactly 1 process, 1 bash child, 1 Hyprland client window, stable over multi-second runs. Most likely it was specific to the dead Mint environment (X11/Cinnamon or its shell config). If it ever resurfaces: check `$SHELL` and what the spawned shell's rc files execute; the app itself contains no window-respawn path (`termite-app/src/lib.rs` opens one `iced::application` and spawns one PTY).

New-environment reconfiguration done along the way: repo-local `git config user.name "Toby"` / `user.email "tobysh08+red@gmail.com"` (the new machine had no git identity); `cargo-audit` 0.22.2 and `cargo-deny` 0.20.2 installed as prebuilt musl binaries in `~/.local/bin`; a `cargo fmt --all` churn commit (`59e45e2`) because the new toolchain's rustfmt disagrees with some hand-aligned code from the old machine.

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
- **Git:** `main`, clean at the end of this session. Everything this session did is committed: `6af31ce` (signer fix), `59e45e2` (rustfmt), `94a2fba` (audit/deny/MSRV). Not pushed — push when the user wants CI to run.
- Verify with `git status` / `git show --stat` rather than trusting this document or commit messages blindly — commits `7a39679`/`906634e` are the standing example of why.

---

## Milestone status

| Milestone | Status | Description |
|-----------|--------|-------------|
| **M0** | ✅ Done | Workspace scaffold, CI, window opens |
| **M1** | ✅ Done | Local terminal emulator (PTY + VT + Iced rendering) — verified end-to-end |
| **M2** | ✅ Done | SSH core (password auth, mandatory known_hosts verification) — hermetic integration test |
| **M3** | 🟡 Core done | Key auth + credential storage: ed25519 load/generate/encrypt/decrypt (`termite-crypto`), `KeyProvider` + `LocalKeyProvider`, publickey auth end-to-end incl. passphrase prompt flow (verified by integration tests), `CredentialStore` on the OS keychain (`termite-storage`, tested). **Remaining** (per `ARCHITECTURE.md` §M3): SSH agent auth (`$SSH_AUTH_SOCK`/Pageant — `AuthMethod::Agent` still fails explicitly), RSA/ECDSA coverage (see hash-alg gap below), `~/.ssh/config` parsing, and the UI-facing bits (passphrase prompt UI, key-gen UI) which land with M4+ |
| M4 | Pending | Host management UI — first real caller for `SessionEvent`/`SessionCommand` |
| M5–M9 | Pending | Tabs, advanced terminal, forwarding/SFTP/ProxyJump, palette, release |

### Known gap for RSA keys (matters when M3 continues)

`KeyProviderSigner::auth_sign` currently **ignores its `hash_alg` parameter** and `LocalKeyProvider::sign` signs with the key's default algorithm. Correct for ed25519 (the only kind `termite-crypto` generates today), but for RSA keys the server negotiates `rsa-sha2-256`/`rsa-sha2-512` and the signature blob's algorithm name must match — a default `ssh-rsa` SHA-1 signature will be rejected. When RSA support lands, thread `hash_alg` through `KeyProvider::sign` (russh's `AgentClient::prepare_sign_request` shows the expected mapping, including the outer contract already implemented in `signer.rs`).

Related note, still true: `russh` pulls `ssh-key 0.7.0-rc.11`; `termite-crypto` uses the same version via `russh::keys` re-export territory — keep them matched to avoid two incompatible `PublicKey` types.

---

## Layout / architecture (unchanged — see v4 in git history for the file-by-file M2 walkthrough)

Workspace: `termite` binary + `crates/{termite-core,-ssh,-terminal,-storage,-crypto,-ui,-app}`. Dependency graph is one-directional, compiler-enforced, `termite-core` at the bottom with no internal deps.

Key M3 files added by the rescued session (all now verified):
- `crates/termite-core/src/traits.rs` — `KeyProvider` (`public_key_blob()`, `sign()`), `CredentialStore` traits.
- `crates/termite-crypto/src/{key,provider,error}.rs` — ed25519 generate/load/decrypt/save (all key material in `ssh_key::PrivateKey`, zeroizing; passphrases only exposed at the decrypt/encrypt call), `LocalKeyProvider`.
- `crates/termite-ssh/src/signer.rs` — `KeyProviderSigner`, the russh `Signer` adapter (see contract note above — it's load-bearing).
- `crates/termite-ssh/src/session.rs` — `authenticate_publickey`: loads key, fires `AuthRequired(Passphrase{fingerprint})` if encrypted, decrypts, authenticates via `authenticate_publickey_with` (never hands russh the raw private key).
- `crates/termite-storage/src/credential_store.rs` — `KeyringStore` (OS keychain via `keyring`) + `MemoryStore` for tests; integration test `tests/keyring_store.rs` hits the real Secret Service.

Security invariants: unchanged from v4/CLAUDE.md, all still honored — secrets in `secrecy` types, no silent host-key accepts (oneshot-approval flow in `handler.rs`), keychain-only persistence, no secrets in logs, openssl banned.

---

## Suggested next steps

1. **Push and watch CI** — this session fixed `deny.toml` for the current cargo-deny and added `.cargo/audit.toml`, but the GitHub actions (`rustsec/audit-check`, `cargo-deny-action`) haven't actually run against it yet.
2. **Finish M3**: agent auth via `$SSH_AUTH_SOCK` (russh has `AgentClient`; present it as an alternative `KeyProvider` per `ARCHITECTURE.md` §"SSH agent integration"), the RSA `hash_alg` threading described above, `~/.ssh/config` parsing.
3. **Or start M4** (host management UI) — it's the first real caller for the whole `SessionEvent`/`SessionCommand` machinery and unblocks the UI-facing M3 leftovers (passphrase prompt dialog, key-gen UI).

---

*Handoff v5 written after fixing the publickey signer, completing full workspace + security verification, and reconfiguring for the new Arch environment. Working agreement: commit actively as work lands (the user asked for this explicitly).*
