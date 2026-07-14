# Termite — Conversation Handoff (v10)

This document gives the next conversation full context to continue development without losing anything. It supersedes v9. **Read this one — the credential prompt now has a working "Save to keychain" toggle: `AuthRequired` checks the keychain first and auto-answers if a saved credential exists, and `Submit` persists one when the toggle is checked. `daeaec6`/`479f05a`/`60f8bfe`/`9c7a409` (v9's local-only commits) remain local-only, and this session added one more local-only commit on top (see Repo state) — nothing has been pushed since `49c59ea`.**

---

## What happened since v9 (2026-07-14, same day — fourth session)

v9 left three optional M4 items on the table (save-to-keychain toggle, `SshConfig` alias resolution, real-`sshd` end-to-end verification) plus the standing question of whether to push. Asked the user which to pick up; they chose the save-to-keychain toggle (`355a4e6`). The other two are still open — see "Suggested next steps" below, now renumbered.

1. **`AuthChallenge::Password` now carries `host`/`username`** (`crates/termite-ssh/src/events.rs`, `session.rs`). Previously a unit variant; `termite-app` needed a way to key a keychain lookup/save without separately tracking which `HostProfile` a session belongs to (no such session→profile map exists — `SessionId` is generated inside `SshSession::spawn`, not known to the app until the first event arrives). Threading `host`/`username` through the challenge itself was simpler than adding that map. `Passphrase`'s existing `fingerprint` field already served the same purpose for key auth, so it was left alone. Updated the two hermetic-integration-test match arms in `termite-ssh/tests/session.rs` accordingly.
2. **`Prompt::Credential` gained a `save: bool` field and a checkbox** (`crates/termite-ui/src/prompt.rs`), plus `PromptMessage::ToggleSave(bool)`. Still pure presentation — the checkbox just tracks a yes/no; `termite-app` decides what "save" means. Defaults unchecked (`save: false` wherever the prompt is opened) — saving a credential is a deliberate per-prompt opt-in, not automatic, consistent with this module's existing framing of the modal as needing "an explicit, unforgeable decision from the user."
3. **Wiring** (`crates/termite-app/src/lib.rs`). New `TermiteApp::credential_store: Arc<dyn CredentialStore>`, built via `KeyringStore::new()` (the real OS keychain — no in-memory fallback the way `host_store` has one, since `KeyringStore` doesn't depend on a config directory existing). Two new helpers: `saved_credential(app, &challenge)` — tries `credential_store.get_password`/`get_passphrase` depending on the challenge variant, returns `None` (not an error) on any lookup failure so a keychain hiccup falls back to prompting rather than breaking the connection; `save_credential(app, &challenge, &secret)` — the write-side counterpart, called from `update_prompt`'s `Submit` arm only when `save` was checked. `handle_session_event`'s `AuthRequired` arm now calls `saved_credential` before opening the modal at all — a hit answers the challenge immediately via `send_to_session` and returns, so a previously-saved credential means the modal never appears on a later connection to the same host. The already-pending-prompt fail-closed check (v9) still runs first, unchanged.
4. **Tests** (`355a4e6`, termite-app going from 14 to 17). `test_app()` now swaps in a `MemoryStore` for `credential_store` after construction (`TomlHostStore`/`KeyringStore` fields are otherwise real, matching v8/v9's precedent for `host_store`) — real `KeyringStore` would technically work too since `cargo test --workspace` already runs under a real Secret Service/Keychain/Credential Manager per `ci.yml`, but routing test credentials like `"hunter2"` through the *actual* OS keychain on whatever machine runs the suite (this dev box included) felt like an unnecessary side effect to accept for a unit test, so `MemoryStore` was used instead — same reasoning as `termite-storage`'s own `MemoryStore`/`KeyringStore` split. New: `submit_with_save_checked_persists_password_to_credential_store`, `submit_without_save_does_not_persist_to_credential_store`, `auth_required_auto_answers_from_a_saved_password_without_prompting`. Existing tests updated only mechanically (`AuthChallenge::Password` → a new `test_password_challenge()` helper carrying dummy host/username, `Prompt::Credential { label, input }` patterns gained `save`).

**Not done / explicitly out of scope this session**: no UI affordance to *delete* a saved credential (the sidebar's host-delete doesn't touch the keychain either — pre-existing gap, unrelated to this session); no visual/click-driven confirmation that the checkbox renders and toggles correctly — the modal is only reachable via a live `AuthRequired`/`HostKeyUnknown` event, which (per the standing "Isolated GUI testing" limitation below) still can't be driven end-to-end in this environment, so this is the same gap v9 already had for the rest of the modal, just extended to the new checkbox. Unit tests are the substitute, as before.

**Verification**: `cargo test --workspace` (52 tests, up from 49), `clippy --all-targets -D warnings` clean, `fmt --all --check` clean. `cargo test -p termite-storage`'s `keyring_store.rs` integration test (pre-existing, unrelated to this session) still passes against the real Secret Service on this dev machine, confirming the environment those tests need is present — but no *new* test in this session touches it, per the `MemoryStore` reasoning above.

---

## What happened since v8 (2026-07-14, same day — third session)

Closed out the gap v8 flagged as the natural next M4 slice: `SessionEvent::AuthRequired`/`HostKeyUnknown`/`HostKeyMismatch` previously failed closed with no UI (disconnect / reject unconditionally). All three now open a real modal:

1. **Auth-method picker** (`crates/termite-ui/src/sidebar.rs`). The add-host form gained an Agent/Password/Public-key button row (`AuthKind`, a UI-local enum — kept separate from `termite_core::AuthMethod` because the form needs a selectable "public key" state before a path has been typed in, which `AuthMethod::PublicKey`'s mandatory `PathBuf` can't represent) and a conditional key-path text field shown only when Public key is selected. `termite-app`'s `update_sidebar` builds the real `AuthMethod` from `(auth_kind, key_path_input)` on submit; a public-key profile with an empty path is rejected client-side rather than saved (would be unfixable short of delete-and-re-add).
2. **Credential + host-key modal** (new `crates/termite-ui/src/prompt.rs`). One pure-presentation module covers both: `Prompt::Credential { label, input }` (a masked `text_input`, Submit/Cancel) and `Prompt::HostKey { label, algorithm, fingerprint, warning }` (Reject/"Trust & continue", red title when `warning` is set for a *changed* key vs. a first-contact one). Deliberately takes only plain strings/bools — never `termite_ssh::{AuthChallenge, HostKey}` — keeping `termite-ui` free of SSH knowledge per `CLAUDE.md`'s crate boundaries. Rendered as a dimmed full-screen overlay via `iced::widget::Stack` in `termite-app`'s `view()`.
3. **Wiring** (`crates/termite-app/src/lib.rs`). New `TermiteApp::pending_prompt: Option<PendingPrompt>`, an app-local enum (`Credential { session, challenge, ui: Prompt }` / `HostKey { session, ui: Prompt }`) that embeds the `Prompt` UI data directly rather than deriving it fresh in `view()` — needed because `prompt::view()`'s returned `Element` borrows its input, and a value derived fresh each frame would be a dangling temporary; embedding it in state owned by `app` gives the borrow `app`'s lifetime instead (`E0515` if you try the derive-on-render approach — hit and fixed this session). `handle_session_event` opens the modal on `AuthRequired`/`HostKeyUnknown`/`HostKeyMismatch`; `update_prompt` answers it (`Submit` → wraps the input in `secrecy::SecretString` and sends `SessionCommand::AuthResponse`; `Approve`/`Reject` → `SessionCommand::ApproveHostKey`). Still fails closed, now scoped correctly: only one prompt is shown at a time — a second one arriving while the modal is already open disconnects/rejects rather than silently overwriting the pending decision. A prompt tied to a session that disconnects out from under it (e.g. the user answered a *different* stale prompt) is cleared automatically rather than left dangling.

4. **Unit tests for the new state machine** (`60f8bfe`). Given neither a live SSH server nor click-driven GUI testing is available here (see below), `handle_session_event`/`update_prompt` are instead exercised directly: a real `TermiteApp` (still spawns a real local-shell PTY — `TermiteApp::new()` doesn't support injecting a fake one) with a fake `ssh_worker` channel standing in for the subscription, so tests assert exactly which `SshWorkerInput` — `Disconnect`, `AuthResponse(Password|Passphrase)`, `ApproveHostKey(bool)` — each path sends, using `bridge::Receiver::try_recv()` (not `try_next`, deprecated in this iced-vendored `futures` version) to poll it synchronously with no executor needed. Covers: both challenge/host-key variants open the right prompt with the right label; a second prompt arriving while one's already open fails closed and leaves the first untouched; `Submit`/`Cancel`/`Approve`/`Reject` all send the right command and clear the prompt; a `Disconnected` event clears a prompt tied to *its own* session but leaves an unrelated one alone. Also extracted `auth_method_from_form(AuthKind, key_path) -> AuthMethod` out of `update_sidebar` so that mapping has its own direct test rather than only being reachable through the async `Task`-returning host-store round trip (which isn't easily unit-testable outside the Iced runtime). 14 new tests, `termite-app` going from 0 to 14.

**Not done / explicitly out of scope this session**: no "save to keychain" toggle on the credential prompt — submitted passwords/passphrases go straight to `SessionCommand::AuthResponse` and are not persisted via `CredentialStore`, so they'll be asked for again next connection. This was already called out as a separate, optional M3/M4 item in v8 and earlier; still true.

**Verification**: `cargo test --workspace` (49 tests, up from 35 at the start of this session) green, `clippy --all-targets -D warnings` clean, `fmt --check` clean, and a rendering smoke test in the isolated Xvfb `:99` display (see below) confirmed the auth-method picker draws correctly with Agent pre-selected. **Still not done**: no live SSH server or click-driven GUI test has exercised `AuthRequired`/`HostKeyUnknown`/`HostKeyMismatch` end-to-end through the actual modal — item 4 above is the closest substitute available in this environment (it tests the same handler functions the real UI calls, just without a real network round-trip or actual mouse clicks). See "Isolated GUI testing"'s standing limitation that only rendering, not interaction, can be screenshot-verified.

One design note worth keeping for later sessions: `iced::keyboard::on_key_press` (used to route keystrokes to the active SSH session / local PTY) only fires for events the widget tree left `Status::Ignored` — a focused `text_input` marks the event `Captured` first, so typing into the sidebar's fields or this new credential prompt does **not** leak keystrokes to the terminal underneath. Checked `iced_futures-0.13.2/src/keyboard.rs` directly to confirm this isn't a latent bug, since the global-subscription-plus-text-inputs combination looked suspicious at a glance.

---

## What happened since v7 (2026-07-14, same day — second session)

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
- **Git:** `main`. `9dac4ee`/`970e428`/`c35fd44` (v8's commits) are pushed. `daeaec6`/`479f05a`/`60f8bfe`/`9c7a409` (v9's commits) and this session's `355a4e6` (save-to-keychain toggle) are all local-only — 5 commits ahead of `origin/main` as of this HANDOFF update. Working agreement: commit actively, push only when asked — no push has been asked for since `49c59ea`. Don't assume local-only commits are on GitHub — CI has not run against them.
- Verify with `git status` / `git show --stat` rather than trusting this document or commit messages blindly — commits `7a39679`/`906634e` are the standing example of why, and `12b8518`'s "unbreak CI" claim (see v7 section below) is the same lesson applied to CI status specifically: don't trust "should work" for a GitHub Actions change, check the actual run.
- Environment: Arch, Hyprland/Wayland, Rust 1.97, repo-local git identity, `cargo-audit`/`cargo-deny` binaries in `~/.local/bin`. No `gh` CLI installed — checked Actions status via unauthenticated `curl` against `api.github.com/repos/brass-ape/termite/actions/runs` (works for run/job status on this public repo; job log downloads 403 without admin token).

---

## Milestone status

| Milestone | Status | Description |
|-----------|--------|-------------|
| **M0** | ✅ Done | Workspace scaffold, CI, window opens |
| **M1** | ✅ Done | Local terminal emulator (PTY + VT + Iced rendering) — verified end-to-end |
| **M2** | ✅ Done | SSH core (password auth, mandatory known_hosts verification) — hermetic integration test |
| **M3** | 🟢 Protocol layer done | Key auth + credential storage. Done: ed25519 generate/load/encrypt/decrypt, RSA loading + correct `rsa-sha2-*` hash selection, passphrase decryption + prompt event flow, `KeyProvider`/`LocalKeyProvider`, publickey auth end-to-end, SSH agent auth, `CredentialStore` on the OS keychain, `~/.ssh/config` parsing, passphrase/password prompt dialog, save-to-keychain toggle on that prompt (this session — see below). **Remaining are UI-facing items that land with M4+**: key-gen UI. ECDSA loading is not enabled (ssh-key `p256` feature undeclared) — deliberate cut unless a user needs it; ed25519/RSA cover the field |
| M4 | 🟡 In progress | Host management UI. Done: `HostStore` persistence, sidebar list/add/delete, `SessionEvent`/`SessionCommand` wiring, auth-method picker in the add-host form, credential prompt modal with a save-to-keychain toggle, host-key-approval modal — password/public-key profiles and first-contact hosts are now reachable end-to-end from the UI, and a saved credential auto-answers a later `AuthRequired` without reprompting. Remaining: `SshConfig` alias resolution (parser exists, nothing calls it yet), key-gen UI |
| M5–M9 | Pending | Tabs, advanced terminal, forwarding/SFTP/ProxyJump, palette, release |

---

## Layout / architecture

Workspace: `termite` binary + `crates/{termite-core,-ssh,-terminal,-storage,-crypto,-ui,-app}`. Dependency graph is one-directional, compiler-enforced, `termite-core` at the bottom with no internal deps. (v4 in git history has the file-by-file M2 walkthrough.)

M4 files so far, all verified:
- `crates/termite-storage/src/host_store.rs` — `HostStore` trait, `TomlHostStore`, `MemoryHostStore`.
- `crates/termite-ui/src/sidebar.rs` — `sidebar::view()`, `SidebarMessage`, `SidebarState`, `AuthKind` (the add-host form's Agent/Password/Public-key picker). Pure presentation, no `HostStore`/SSH knowledge.
- `crates/termite-ui/src/prompt.rs` — `prompt::view()`, `Prompt` (`Credential { label, input, save }`/`HostKey` variants), `PromptMessage` (incl. `ToggleSave(bool)`). Pure presentation, only ever sees plain display strings and a `bool` — no `termite_ssh` types.
- `crates/termite-app/src/lib.rs` — `TermiteApp` now holds `host_store`, `hosts`, `sidebar`, `credential_store: Arc<dyn CredentialStore>`, `ssh_worker: Option<Sender<SshWorkerInput>>`, `active_session: Option<SessionId>`, `pending_prompt: Option<PendingPrompt>`. The `ssh_worker` free fn is the persistent subscription described above; `handle_session_event` is where `SessionEvent`s land (now also checking `saved_credential` before opening the modal), `update_prompt` is where the modal's answer is turned into a `SessionCommand` (and, on `Submit` with `save` checked, into a `save_credential` call).

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

1. **End-to-end verification against a real server** — the state-machine logic (including the save/auto-answer path added this session) has unit coverage, but nothing has exercised `AuthRequired`/`HostKeyUnknown`/`HostKeyMismatch` against an actual `sshd` and clicked through the resulting modal with a real mouse/window. `sshd` exists on this box (`/usr/sbin/sshd`) but its systemd service is deliberately left untouched (shared system state — see earlier verification-limits notes); a user-scoped `sshd -f <config> -p <high port> -D` bound to `127.0.0.1` with its own host key and `AuthorizedKeysFile`, run as the current user, would be hermetic and worth trying next time — same spirit as `termite-ssh`'s existing in-process test agent/server. Click-driving the resulting modal is still blocked by the `ydotool` leak-into-real-session problem (see "Isolated GUI testing"); no substitute has been found.
2. **`SshConfig` alias resolution** — the `~/.ssh/config` parser from M3 (`crates/termite-ssh/src/ssh_config.rs`) still has no caller; the natural hook is resolving an alias when the add-host form's address field is typed.
3. **No UI to delete a saved credential** — the new save-to-keychain toggle has no corresponding "forget this password" action; deleting a host in the sidebar doesn't touch the keychain either (`CredentialStore::delete_password`/`delete_passphrase` exist and are tested in `termite-storage` but nothing in `termite-app` calls them yet). Worth pairing with host deletion or its own explicit action once there's UI real estate for it.
4. **Decide whether to push** — 5 commits are local-only (`daeaec6`, `479f05a`, `60f8bfe`, `9c7a409`, `355a4e6`); see Repo state.

---

*Handoff v10 written after adding a "Save to keychain" toggle to the credential prompt — v9's top pick among three optional M4 items, chosen by the user directly. `AuthChallenge::Password` now carries `host`/`username` so a save/lookup can be keyed without a session→profile map; a saved credential now auto-answers `AuthRequired` without reprompting. v9's auth-picker/prompt-modal content is kept above for history. Working agreement unchanged: commit actively as work lands; push only when asked.*
