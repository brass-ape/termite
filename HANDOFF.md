# Termite — Conversation Handoff (v8)

This document gives the next conversation full context to continue development without losing anything. It supersedes v7. **Read this one — M4 (host management) is now underway: host profiles persist, the sidebar renders and edits them, and selecting a host actually opens a real SSH session. Three commits are local-only, not yet pushed (see Repo state).**

---

## What happened since v7 (2026-07-14, same day)

M4 work started, in dependency order per `ARCHITECTURE.md`'s checklist (host profile CRUD → sidebar → session wiring):

1. **`HostStore`** (`7a459b7`). New `crates/termite-storage/src/host_store.rs`: a `HostStore` trait (`list`/`get`/`save`/`delete`) mirroring the existing `CredentialStore` pattern, with `TomlHostStore` (disk-backed, `~/.config/termite/hosts.toml`, re-reads/re-writes the whole file every call — deliberately not cached, to avoid staleness vs. hand-edits) and `MemoryHostStore` for tests. 4 unit tests.
2. **Sidebar UI** (`9dac4ee`). New `crates/termite-ui/src/sidebar.rs`: pure-presentation `sidebar::view()` — a scrollable host list plus an "add host" form — emitting `SidebarMessage`. `termite-app` wires it up: loads hosts asynchronously on startup (`spawn_blocking`, same pattern as settings), persists add/delete through `HostStore`. Verified by rendering (not just compiling) in an isolated Xvfb `:99` display — see "Isolated GUI testing" below for why and how.
3. **`SessionCommand`/`SessionEvent` wiring** (`c35fd44`, this session). `SidebarMessage::SelectHost` now actually connects. A persistent `Subscription` (`ssh_worker` in `termite-app/src/lib.rs`) owns every spawned `SshSession` keyed by `SessionId`, using iced's documented bidirectional-subscription-worker pattern: it hands the app a `Sender<SshWorkerInput>` via `Message::SshWorkerReady` on first poll (the app can't construct the stream's internal channel itself — the stream owns it), then multiplexes `SessionEvent`s back through `Message::SessionEvent`. Keystrokes route to the active session's channel when connected, else the local PTY as before. Connection lifecycle (connected/disconnected/error) is appended as plain text into the terminal grid — there's no dedicated status UI until tabs land in M5, and that's the only visible surface right now.

**Deliberately deferred, fails closed for now**: `SessionEvent::AuthRequired` and `HostKeyUnknown`/`HostKeyMismatch` have no prompt UI yet. Per `CLAUDE.md`'s no-silent-accept invariant, `handle_session_event` disconnects on an auth challenge and rejects unrecognized/changed host keys rather than guessing — it does not (and must not) auto-approve. This isn't reachable from today's UI anyway: the sidebar's "add host" form has no auth-method picker, so `HostProfile::new`'s default (`AuthMethod::Agent`) is all it ever creates, and agent auth doesn't raise `AuthRequired`. The natural next M4 slice is: an auth-method picker in the add-host form, a passphrase/password prompt dialog, and a host-key-approval dialog — all three need real UI before password/publickey profiles or first-contact hosts are usable end-to-end.

**Verification limits**: no live SSH server was available in this environment to exercise the connect path end-to-end (`sshd` isn't running, and starting it would touch shared system state). `termite-ssh`'s own hermetic integration tests (`tests/session.rs`) already cover the protocol logic this wiring calls into and pass. What *was* verified: `cargo test --workspace` all green, `clippy -D warnings` clean, `fmt` clean, and a rendering smoke test.

**Isolated GUI testing**: running the app for a visual check must not put a window on the user's real desktop. Earlier in this project's history the app was accidentally launched directly and appeared in the user's live Hyprland session — killed immediately once noticed. The safe pattern used since: `Xvfb :99 -screen 0 1280x800x24` (via the Bash tool's `run_in_background`, not shell `&`/`disown` — backgrounding via raw `&` in this sandbox produced a spurious exit 144 on the whole tool call), then `WAYLAND_DISPLAY= DISPLAY=:99 WINIT_UNIX_BACKEND=x11 ./target/debug/termite`, screenshotted via `DISPLAY=:99 import -window root`. **Do not use `ydotool`** to simulate clicks for interaction testing — it injects through `/dev/uinput`, a system-wide kernel input device not scoped to the `:99` display, so it would leak input into the user's real session. This means only rendering can be verified this way, not click-driven interaction; no substitute tool has been found yet.

---

## What happened since v6 (2026-07-14) — history, superseded above

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
- **Git:** `main`, working tree clean, but **3 commits ahead of `origin/main`** (`9dac4ee`, `970e428`, `c35fd44` — HostStore, a CLAUDE.md doc sync, and SSH session wiring). Not pushed: working agreement is commit actively, push only when asked, and no push has been asked for since `7a459b7`/`49c59ea`. Don't assume these are on GitHub — CI has not run against them.
- Verify with `git status` / `git show --stat` rather than trusting this document or commit messages blindly — commits `7a39679`/`906634e` are the standing example of why, and `12b8518`'s "unbreak CI" claim (see v7 section below) is the same lesson applied to CI status specifically: don't trust "should work" for a GitHub Actions change, check the actual run.
- Environment: Arch, Hyprland/Wayland, Rust 1.97, repo-local git identity, `cargo-audit`/`cargo-deny` binaries in `~/.local/bin`. No `gh` CLI installed — checked Actions status via unauthenticated `curl` against `api.github.com/repos/brass-ape/termite/actions/runs` (works for run/job status on this public repo; job log downloads 403 without admin token).

---

## Milestone status

| Milestone | Status | Description |
|-----------|--------|-------------|
| **M0** | ✅ Done | Workspace scaffold, CI, window opens |
| **M1** | ✅ Done | Local terminal emulator (PTY + VT + Iced rendering) — verified end-to-end |
| **M2** | ✅ Done | SSH core (password auth, mandatory known_hosts verification) — hermetic integration test |
| **M3** | 🟢 Protocol layer done | Key auth + credential storage. Done: ed25519 generate/load/encrypt/decrypt, RSA loading + correct `rsa-sha2-*` hash selection, passphrase decryption + prompt event flow, `KeyProvider`/`LocalKeyProvider`, publickey auth end-to-end, SSH agent auth, `CredentialStore` on the OS keychain, `~/.ssh/config` parsing. **Remaining are UI-facing items that land with M4+**: passphrase prompt dialog, key-gen UI, optional passphrase/password save toggles. ECDSA loading is not enabled (ssh-key `p256` feature undeclared) — deliberate cut unless a user needs it; ed25519/RSA cover the field |
| M4 | 🟡 In progress | Host management UI. Done: `HostStore` persistence, sidebar list/add/delete, `SessionEvent`/`SessionCommand` wiring (session actually connects on select). Remaining: auth-method picker in the add-host form, passphrase/password prompt dialog, host-key-approval dialog (all three needed before non-agent auth or first-contact hosts work end-to-end), `SshConfig` alias resolution, key-gen UI |
| M5–M9 | Pending | Tabs, advanced terminal, forwarding/SFTP/ProxyJump, palette, release |

---

## Layout / architecture

Workspace: `termite` binary + `crates/{termite-core,-ssh,-terminal,-storage,-crypto,-ui,-app}`. Dependency graph is one-directional, compiler-enforced, `termite-core` at the bottom with no internal deps. (v4 in git history has the file-by-file M2 walkthrough.)

M4 files so far, all verified:
- `crates/termite-storage/src/host_store.rs` — `HostStore` trait, `TomlHostStore`, `MemoryHostStore`.
- `crates/termite-ui/src/sidebar.rs` — `sidebar::view()`, `SidebarMessage`, `SidebarState`. Pure presentation, no `HostStore`/SSH knowledge.
- `crates/termite-app/src/lib.rs` — `TermiteApp` now holds `host_store`, `hosts`, `sidebar`, `ssh_worker: Option<Sender<SshWorkerInput>>`, `active_session: Option<SessionId>`. The `ssh_worker` free fn is the persistent subscription described above; `handle_session_event` is where `SessionEvent`s land.

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

1. **Auth-method picker + prompt dialogs** — the sidebar's add-host form only ever creates `AuthMethod::Agent` profiles, and `AuthRequired`/`HostKeyUnknown`/`HostKeyMismatch` currently fail closed with no UI (see above). Needed together: a way to pick Password/PublicKey/Agent when adding a host, a passphrase/password prompt dialog wired to `AuthResponse`, and a host-key-approval dialog wired to `ApproveHostKey` — otherwise nothing but agent auth against already-trusted hosts is reachable from the UI.
2. **Decide whether to push** the 3 local-only commits (`9dac4ee`, `970e428`, `c35fd44`) — nothing has asked for a push since `49c59ea`.

---

*Handoff v8 written after wiring real `SessionEvent`/`SessionCommand` handling into `termite-app` on top of the HostStore/sidebar work from earlier the same day. v7's CI-fix content is kept below for history. Working agreement: commit actively as work lands (the user asked for this explicitly); push only when asked.*
