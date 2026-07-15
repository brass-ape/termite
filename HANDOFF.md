# Termite — Conversation Handoff (v13)

This document gives the next conversation full context to continue development without losing anything. It supersedes v12. **Read this one — M4 (Host Management) is now done in full against `ARCHITECTURE.md`'s checklist: favourites, recent connections, search/filter, host tags, a profile editor (create *and* update, not just create/delete), `~/.ssh/config` bulk import, and key-generation UI all landed this session on top of what v5–v12 already had. M3's only remaining gap (key-gen UI) closed too. 11 commits ahead of `origin/main` (see Repo state) — nothing has been pushed since `49c59ea`.**

This handoff has been compacted: v5–v12's session-by-session narration is condensed into "Project history" below. Read that section for load-bearing lessons (the signer contract, CI gotchas, testing methodology); read git log / `git show --stat` for exact diffs if you need more than this document has.

---

## What happened this session (2026-07-15, v13)

Prior session (v12) closed the delete-saved-credential gap and left three items open: real-`sshd` end-to-end verification, a standalone forget-credential action, and `ProxyJump` consumption. This session's ask was broader — "continue through the rest of your current M" — so instead of picking one item off that list, it went back to `ARCHITECTURE.md`'s M4 checklist directly and closed every item that wasn't already done. Two commits:

**`36d9db3` — groundwork.** `HostProfile` (`crates/termite-core/src/types.rs`) gains `favourite: bool` and `last_connected: Option<u64>` (unix seconds), both `#[serde(default)]` so a `hosts.toml` written before these fields existed still loads — verified with a hand-written legacy-file fixture in a new `termite-storage` test.

**`2e6ed3d` — the rest of M4, in one commit** (interdependent: all of it lives in `sidebar::view()`/`update_sidebar()`, splitting further would have meant repeatedly re-diffing the same functions):

1. **Favourites** — a star toggle in each host row (`SidebarMessage::ToggleFavourite`), persisted on `HostProfile.favourite`.
2. **Recent connections** — `HostProfile.last_connected` stamped by `SidebarMessage::SelectHost` on every connect attempt (not just successful ones — stamping on attempt, not on the `Connected` event, keeps this in the synchronous message handler rather than needing session-event plumbing back to a specific host).
3. **Sidebar ordering** — new `sort_hosts()` applied whenever `Message::HostsLoaded` delivers a fresh list: favourites first, then most-recently-connected, never-connected last, alphabetical tiebreak. `sidebar::view()` itself still just renders whatever order it's given — this is the one place display order is decided, consistent with its pre-existing "pure presentation" contract.
4. **Search/filter** — a search box filters by name/host/username/tag, case-insensitive substring, entirely inside `sidebar::view()` (display-only, no caller round-trip needed).
5. **Host tags** — `HostProfile.tags` existed since M4 started but had no UI; now a comma-separated field in the form (`parse_tags()` splits/trims it), shown under each host row, included in the search filter.
6. **Profile editor** — an "edit" button loads a host back into the form (`SidebarMessage::EditHost` → `load_profile_into_form()`); `AddHost` is renamed `SaveHost` and now creates *or* updates depending on whether a host is being edited, explicitly preserving `id`/`favourite`/`last_connected` on update since the form doesn't surface any of the three.
7. **Host import from `~/.ssh/config`** — new `SshConfig::host_aliases()` (`crates/termite-ssh/src/ssh_config.rs`) lists literal, non-wildcard `Host` patterns (aliases come back lowercased — the parser only retains the lowercased form of each pattern). `SidebarMessage::ImportFromSshConfig` saves a profile for every alias not already present as a saved host (matched by name), resolved through the existing `SshConfig::query`.
8. **Key generation UI** — inline in the public-key auth picker: optional comment + passphrase fields and a "Generate key" button. Keys are written under the app's own config directory by default (`default_keygen_path()` — never the user's real `~/.ssh` unless they type a path there explicitly), and generation refuses to ever overwrite an existing file at the target path (checked explicitly — `ssh_key::PrivateKey::write_openssh_file` truncates blindly, it does **not** refuse on its own). A passphrase given at generation time is saved to the keychain immediately, since unlike an existing key nobody has had a chance to use the credential prompt's own save-to-keychain toggle for it.

Also extracted `termite_crypto::key::fingerprint()` from `termite-ssh`'s inline computation (`loaded.public_key().fingerprint(HashAlg::Sha256)`), so `termite-app` can turn a key path back into a fingerprint too (needed for the keychain save in item 8) without taking a direct `ssh_key`/`russh` dependency.

**Tests**: 17 new across three crates (`termite-app` 29 → 46, plus one in `termite-ssh` for `host_aliases`, one in `termite-storage` for the legacy-file load). Every new free function (`sort_hosts`, `parse_tags`, `load_profile_into_form`, `host_profile_from_config`, `first_available_key_path`, `generate_and_save_key`) is tested directly. Message-level tests for `SaveHost`/`ToggleFavourite`/`SelectHost`/`ImportFromSshConfig` only assert what happens *synchronously before* the handler's `Task::perform(spawn_blocking(...), Message::HostsLoaded)` is returned (form resets, `ssh_worker` sends) — that Task is never polled by a plain `#[test]` fn (no Iced runtime or `#[tokio::test]` driving it), same limitation `delete_host_forgets_its_saved_password` already worked around in v12. `GenerateKey` has no such gap: it has no `Task`, so it's tested fully end-to-end including real file-system effects in a `tempfile::tempdir`.

**Rendering verification** (Xvfb `:99`, per the standing method below) caught and fixed one real bug: the ★/☆ favourite glyphs (`\u{2605}`/`\u{2606}`) don't exist in iced's default UI font and rendered as missing-glyph boxes. Replaced with plain ASCII `"*"`/`"o"`, confirmed by a second screenshot against a scratch `XDG_CONFIG_HOME` seeded with a `hosts.toml` (two hosts, one favourited, with tags) — this is a reusable technique for rendering-verifying host-list state without touching the user's real `~/.config/termite/`.

**Not done / explicitly out of scope**: no click-driven verification of the star toggle, edit button, or Generate-key button actually being pressed — the standing `ydotool`/`/dev/uinput` limitation (below) still applies; unit tests are the substitute, as they have been since v8. `ProxyJump` is still not consumed (M7-scoped). No standalone "forget credential without deleting the host" action (v12 already flagged this; still open). No live-`sshd` end-to-end test.

**Verification**: `cargo test --workspace` all green (46/10/15/4/8/3 across the crates with tests), `clippy --all-targets -D warnings` clean, `fmt --all --check` clean, `cargo audit`/`cargo deny check` clean.

---

## Project history (v5–v12, condensed)

- **M0–M2** (earliest sessions): workspace scaffold + CI, local terminal emulator (PTY/VT/Iced), SSH core with mandatory `known_hosts` verification. All verified end-to-end; not revisited since.
- **M3 protocol layer** (v5–v6): RSA hash-algorithm threading (`rsa-sha2-256`/`512`, negotiated not guessed), SSH agent auth (`$SSH_AUTH_SOCK`, key material never enters the process), `~/.ssh/config` parsing (own parser, no crate dep — OpenSSH resolution semantics: first-value-wins, `IdentityFile` accumulates, fnmatch patterns, `Match` blocks inert). **Load-bearing gotcha**: `KeyProviderSigner::auth_sign` (`crates/termite-ssh/src/signer.rs`) must return the *entire* `to_sign` buffer with the signature appended as a length-prefixed SSH string, not bare signature bytes — russh slices the userauth packet out of it; getting this wrong hangs the session silently.
- **CI** (v6–v7): `rustsec/audit-check@v2` has its own `ignore:` action input separate from `.cargo/audit.toml` — the two must be kept in sync manually (now also documented in `CLAUDE.md`). The action also needs an explicit `checks: write` permission or it 403s publishing results even when the audit itself passes. Lesson: a commit message claiming a GitHub Actions change is "verified" isn't evidence — check the actual run (`gh` isn't installed here; unauthenticated `curl` against `api.github.com` works for public-repo run/job status).
- **M4 UI, part 1** (v7–v9): `HostStore` (TOML-backed, re-reads the whole file every call rather than caching, to avoid staleness vs. hand-edits), the sidebar list/add/delete, `SessionCommand`/`SessionEvent` wiring via a persistent Iced subscription (`ssh_worker`), then the auth-method picker + credential/host-key prompt modal (`AuthRequired`/`HostKeyUnknown`/`HostKeyMismatch` previously failed closed with no UI at all), then a save-to-keychain toggle on that prompt.
- **M4 UI, part 2** (v10–v12): `~/.ssh/config` `Host`-alias resolution in the add-host form (Enter on the address field), then deleting a host also forgetting its saved keychain credential (this closed a real gap: `CredentialStore` had no `delete_passphrase` at all before v12, only `delete_password`).
- **Testing methodology, established v7–v8 and unchanged since**: no live SSH server here (`sshd` exists but its systemd service is deliberately left untouched — shared system state); `termite-ssh`'s hermetic integration tests substitute. No click-driven GUI testing — see "Isolated GUI testing" below. `handle_session_event`/`update_sidebar`/`update_prompt` are instead exercised directly against a real `TermiteApp` with a fake `ssh_worker` channel standing in for the subscription.

**Isolated GUI testing** (unchanged since v7, still the only way to visually verify anything): running the app for a visual check must never put a window on the user's real desktop — it did once, by accident, early in this project, and was killed immediately once noticed. The safe pattern: `Xvfb :99 -screen 0 1280x800x24` (via the Bash tool's `run_in_background`, not shell `&`/`disown` — raw `&` backgrounding in this sandbox produces a spurious exit 144 on the whole tool call), then `WAYLAND_DISPLAY= DISPLAY=:99 WINIT_UNIX_BACKEND=x11 ./target/debug/termite`, screenshotted via `DISPLAY=:99 import -window root`. To seed host-list state for a screenshot without touching the user's real config, launch with `XDG_CONFIG_HOME=<scratch dir>` pointing at a scratch directory containing a hand-written `termite/hosts.toml` (new this session — see above). **Never use `ydotool`** to simulate clicks — it injects through `/dev/uinput`, a system-wide kernel input device not scoped to the `:99` display, so it would leak input into the user's real session. Only rendering can be verified this way, not click-driven interaction; no substitute has been found.

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
- **Git:** `main`. `9dac4ee`/`970e428`/`c35fd44` (v8) are pushed. Everything since — `daeaec6`/`479f05a`/`60f8bfe`/`9c7a409`/`355a4e6` (v9/v10), `cb0879d` (v11), `3dc07ae` (v12), and this session's `36d9db3`/`2e6ed3d` — is local-only: 11 commits ahead of `origin/main`. Working agreement: commit actively, push only when asked — no push has been asked for since `49c59ea`. Don't assume local-only commits are on GitHub — CI has not run against them.
- Verify with `git status` / `git show --stat` rather than trusting this document or commit messages blindly — commits `7a39679`/`906634e` (early history) and `12b8518`'s "unbreak CI" claim are the standing examples of why.
- Environment: Arch, Hyprland/Wayland, Rust 1.97, repo-local git identity, `cargo-audit`/`cargo-deny` binaries in `~/.local/bin`. No `gh` CLI installed.

---

## Milestone status

| Milestone | Status | Description |
|-----------|--------|-------------|
| **M0** | ✅ Done | Workspace scaffold, CI, window opens |
| **M1** | ✅ Done | Local terminal emulator (PTY + VT + Iced rendering) — verified end-to-end |
| **M2** | ✅ Done | SSH core (password auth, mandatory known_hosts verification) — hermetic integration test |
| **M3** | 🟢 Done except one deliberate cut | Key auth + credential storage. All of `ARCHITECTURE.md`'s checklist done, including key-gen UI (this session). ECDSA loading is not enabled (`ssh-key`'s `p256` feature undeclared) — deliberate cut unless a user needs it; ed25519/RSA cover the field |
| **M4** | ✅ Done | Host management. All of `ARCHITECTURE.md`'s checklist done: CRUD (create/read/update/delete), sidebar list, favourites, recent connections, search/filter, host tags, `~/.ssh/config` import, profile editor UI |
| M5–M9 | Pending | Tabs, advanced terminal, forwarding/SFTP/ProxyJump, palette, release. **M5 (Tabs & Multi-Session) is the natural next milestone** |

---

## Layout / architecture

Workspace: `termite` binary + `crates/{termite-core,-ssh,-terminal,-storage,-crypto,-ui,-app}`. Dependency graph is one-directional, compiler-enforced, `termite-core` at the bottom with no internal deps.

M4 files, all verified:
- `crates/termite-storage/src/host_store.rs` — `HostStore` trait, `TomlHostStore`, `MemoryHostStore`.
- `crates/termite-ui/src/sidebar.rs` — `sidebar::view()`, `SidebarMessage` (now: `ResolveAlias`, `SaveHost`, `CancelEdit`, `EditHost`, `ToggleFavourite`, `ImportFromSshConfig`, `TagsInputChanged`, `SearchInputChanged`, keygen fields — see this session's work above), `SidebarState`, `AuthKind`. Pure presentation throughout — no `HostStore`/`CredentialStore`/SSH types leak in; `sidebar::view()` renders whatever host order and filter it's given, doesn't decide either.
- `crates/termite-ui/src/prompt.rs` — `prompt::view()`, `Prompt` (`Credential { label, input, save }`/`HostKey`), `PromptMessage`. Pure presentation.
- `crates/termite-app/src/lib.rs` — `TermiteApp` holds `host_store`, `hosts` (kept sorted via `sort_hosts()`, called from `Message::HostsLoaded`), `sidebar`, `ssh_config: SshConfig`, `credential_store: Arc<dyn CredentialStore>`, `ssh_worker`, `active_session`, `pending_prompt`. Key free functions: `handle_session_event`/`update_prompt` (the auth/host-key modal state machine), `update_sidebar` (everything host-list-related), `apply_resolved_config`/`load_profile_into_form`/`host_profile_from_config` (the three ways form state gets populated from elsewhere), `forget_credential`/`generate_and_save_key` (the two places `termite-app` talks to the keychain directly, alongside `saved_credential`/`save_credential`).

M3 files, all verified:
- `crates/termite-core/src/traits.rs` — `KeyProvider`, `CredentialStore` (now incl. `delete_passphrase`, added v12).
- `crates/termite-crypto/src/{key,provider,error}.rs` — key generate/load/decrypt/save/`fingerprint()` (extracted this session), `LocalKeyProvider`.
- `crates/termite-ssh/src/signer.rs` — `KeyProviderSigner`; see the `auth_sign` contract note above.
- `crates/termite-ssh/src/agent.rs` — `$SSH_AUTH_SOCK` agent auth.
- `crates/termite-ssh/src/ssh_config.rs` — the config parser, now incl. `host_aliases()` (this session).
- `crates/termite-ssh/src/session.rs` — `authenticate_publickey` (passphrase prompt flow), agent dispatch.
- `crates/termite-storage/src/credential_store.rs` — `KeyringStore` + `MemoryStore`; integration test hits the real Secret Service.

Security invariants: unchanged from `CLAUDE.md`, all honored — secrets in `secrecy` types, no silent host-key accepts, keychain-only persistence, no secrets in logs, openssl banned, agent auth never touches key bytes, key generation never overwrites an existing file.

---

## Suggested next steps

1. **Start M5 (Tabs & Multi-Session)** — M4 is done; this is the next milestone in `ARCHITECTURE.md`'s plan. Tab bar, open/close/reorder tabs, per-tab session isolation, keyboard shortcuts (Ctrl+Tab, Ctrl+1–9), session-level reconnect with back-off, a status indicator per tab. `TermiteApp` currently assumes exactly one terminal grid / one active session; this will need to become a collection.
2. **End-to-end verification against a real server** — still nothing has exercised `AuthRequired`/`HostKeyUnknown`/`HostKeyMismatch`, or now the sidebar's new session-wiring-adjacent features, against an actual `sshd`. A user-scoped `sshd -f <config> -p <high port> -D` bound to `127.0.0.1` with its own host key and `AuthorizedKeysFile` would be hermetic and worth trying — same spirit as `termite-ssh`'s existing in-process test agent/server.
3. **No standalone "forget this credential" action** independent of deleting the whole host (e.g. to force a re-prompt after a password changes server-side). Flagged since v12; still open.
4. **`ProxyJump` still isn't consumed anywhere** — parsed (`ssh_config.rs`) and resolved (`apply_resolved_config`/`host_profile_from_config` both see `HostConfig::proxy_jump` and drop it) but nothing acts on it; `HostProfile` has no jump-host concept. Real support is M7-scoped.
5. **Decide whether to push** — 11 commits are local-only; see Repo state.

---

*Handoff v13 written after finishing M4 in full (favourites, recent connections, search/filter, host tags, profile editor, `~/.ssh/config` import, key-gen UI) and compacting v5–v12's session-by-session history into "Project history" above. Working agreement unchanged: commit actively as work lands; push only when asked.*
