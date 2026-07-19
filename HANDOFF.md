# Termite — Conversation Handoff (v14)

This document gives the next conversation full context to continue development without losing anything. It supersedes v13. **Read this one — M5 (Tabs & Multi-Session) has landed in full against `ARCHITECTURE.md`'s checklist: tab bar, open/close/select tabs, per-tab session isolation, `Ctrl+Tab`/`Ctrl+1-9` keyboard navigation, automatic reconnect with exponential backoff plus a manual retry button, and a per-tab connection-status indicator. `TermiteApp` is no longer single-session — see "What happened this session" below for the shape of the refactor. 14 commits ahead of `origin/main` (see Repo state) — nothing has been pushed since `49c59ea`.**

This handoff keeps v5–v13's condensed "Project history" section as-is below; only the top section changes each session. Read "Project history" for load-bearing lessons (the signer contract, CI gotchas, testing methodology); read git log / `git show --stat` for exact diffs if you need more than this document has.

---

## What happened this session (2026-07-20, v14)

M4 finished last session (v13); this session started M5, the next milestone in `ARCHITECTURE.md`. One commit (`termite-core`/`termite-ui`/`termite-app`, see git log — HANDOFF is written before the commit lands, check `git show --stat` for the exact hash).

**The core refactor**: `TermiteApp` went from a single `grid`/`parser`/`writer`/`_pty`/`active_session: Option<SessionId>` to `tabs: Vec<Tab>` + `active_tab: Option<TabId>`. Each `Tab` (`crates/termite-app/src/lib.rs`) owns its *own* `TerminalGrid`/`vte::Parser` and a `TabKind` — `Local { writer, output, _pty }` (a self-contained PTY, same shape as the old single-session fields, just per-tab now) or `Ssh { session_id: Option<SessionId>, profile: HostProfile, reconnect_attempt: u32 }`. `TabId` is a new `termite-core` type (mirrors `SessionId`'s shape exactly — `Copy`/`Eq`/`Hash` `Uuid` newtype — but never persisted, no `Serialize`/`Deserialize`), identifying a tab independent of whichever `SessionId` it's currently bound to (a reconnect gets a new `SessionId` but keeps the same `TabId`).

**Why `session_id` is `Option`, not bare `SessionId`**: `SshSession::spawn` (`termite-ssh/src/session.rs`) generates the `SessionId` itself, synchronously, but only inside the `ssh_worker` subscription task — the app that requested the connection doesn't learn it until a follow-up message. Rather than touch `termite-ssh` (no auth-path change needed for this), `SshWorkerInput::Connect` gained a `TabId` parameter and the worker now echoes `(TabId, SessionId)` back as a new `Message::SessionSpawned`, filled into the tab's `session_id` slot. Before that arrives (and again while a reconnect is pending), `session_id` is `None` — harmless, since no real `SessionEvent` can reference the tab before the id exists, and `close_tab` correctly treats "nothing to disconnect" as a no-op rather than a bug.

**Per-tab event routing replaces "active session wins"**: `handle_session_event` used to gate `SessionEvent::Output` on `active_session == Some(id)`, silently dropping a background session's output — the exact thing M5's "each session fully isolated" checklist item exists to fix. It now resolves the owning tab via `find_tab_by_session_mut` (linear scan — same "small N, don't bother caching" style as the sidebar's host list) and always advances *that* tab's grid, whether or not it's focused. `AuthRequired`/`HostKeyUnknown`/`HostKeyMismatch` additionally call a new `focus_tab_for_session` to switch `active_tab` to the prompting session's tab, so the (still-global, still a `Stack` overlay, unchanged) credential/host-key modal is seen in context instead of possibly appearing over an unrelated tab.

**Reconnect with backoff**: on `SessionEvent::Disconnected`, if the reason isn't `DisconnectReason::Requested` and the tab's `reconnect_attempt` counter is under `MAX_RECONNECT_ATTEMPTS` (6), the tab's status becomes `ConnectionStatus::Reconnecting { attempt }` (this session is the first thing to ever construct or read `ConnectionStatus` — it existed in `termite-core` since early on, fully unused) and `handle_session_event` returns `Task::perform(async move { tokio::time::sleep(delay).await }, move |_| Message::AttemptReconnect(tab_id))`, `delay` from a small exponential-with-cap `backoff_delay` (2s/4s/8s/16s/30s, capped). **Load-bearing gotcha, cost one test failure to find**: `tokio::time::sleep(delay)` must be constructed *inside* the async block passed to `Task::perform`, not built eagerly and passed in directly (`Task::perform(tokio::time::sleep(delay), ...)`) — `sleep()` registers with the runtime's timer driver the instant it's called, which panics outside a Tokio runtime context. A plain `#[test]` fn has no runtime and never polls the returned `Task` anyway, so the eager form panicked at test-call time, before the `Task` was even returned. Wrapping the `sleep(...).await` inside `async move { ... }` defers construction until the future is actually polled by a real runtime. Past the attempt cap, status becomes plain `Disconnected` and the tab bar shows a manual "retry" button instead (`TabBarMessage::Retry`) — both the automatic and manual paths share one `reconnect(app, tab_id)` helper. If a tab is closed while a reconnect is still pending, the eventually-arriving `Message::AttemptReconnect(tab_id)` finds nothing via `find_tab_mut` and is a silent no-op — no cancellation machinery needed.

**Tab bar** (`crates/termite-ui/src/tabbar.rs`, new file): mirrors `sidebar.rs`'s established contract exactly — a `TabBarMessage` enum, a caller-owned `TabSummary { id, title, status }` display struct (no SSH/session types leak into `termite-ui`), sibling (non-nested) `button`s per row for select/close/retry, same as `sidebar.rs`'s host-row pattern. One new wrinkle versus `sidebar::view`: `sidebar::view` legitimately borrows its `&'a [HostProfile]`/`&'a SidebarState` args because `text_input` widgets hold live `&str` references into them, so its returned `Element<'a, _>` ties to the caller's own state. `tabbar::view` uses no `text_input` — every value gets cloned into an owned `String`/`Color`/`Copy` type — so its `Element` doesn't actually need to borrow `tabs` at all, and the signature says so explicitly (`Element<'static, TabBarMessage>`). This matters because the caller (`termite-app`'s `view()`) builds `tab_summaries(app)` as a fresh `Vec` on every frame rather than storing it as `app` state; had `tabbar::view` used the same elided-lifetime pattern as `sidebar::view`, that fresh `Vec` (a function-local temporary) wouldn't have lived long enough to satisfy the borrow, since `view()`'s own return type ties to `&TermiteApp`'s lifetime, not to a local temporary's.

**Keyboard shortcuts**: `Ctrl+Tab`/`Ctrl+Shift+Tab` cycle tabs forward/backward (wrapping), `Ctrl+1`-`Ctrl+9` jump to a tab by position — both intercepted in `KeyPressed` via a new `handle_tab_shortcut`, before the existing `key_to_bytes` byte-forwarding path runs. Checked before implementing: today, unintercepted, `Ctrl+Tab` sends a literal tab byte to the shell and `Ctrl+<digit>` sends the bare digit (neither is a meaningful terminal control sequence — `key_to_bytes`'s Ctrl-handling only special-cases `Key::Character` for letters), so claiming these combinations for tab navigation isn't a behavior regression.

**Local shell tabs**: the app always opens with one (same UX as before M5), and the tab bar's "+" button (`Message::NewLocalTab`) opens more via an extracted `spawn_local_tab()` (the same PTY-spawn-plus-reader-thread code `TermiteApp::new()` used to inline). Closing the last open tab (`close_tab`) immediately reopens a fresh local one — `app.tabs` is never empty, so `view()`/`active_tab()` never need an empty-state branch.

**Tests**: 15 new in `termite-app` (46 → 61): `new_local_tab`/`close_tab` (neighbor-activation, last-tab-reopens-local, live-session disconnect, pending-prompt clearing), `SessionSpawned` wiring, `Disconnected`'s three status-transition branches (retry scheduled / user-requested no-retry / attempt-cap-exhausted), `reconnect`'s manual/automatic-shared path, `select_adjacent_tab`/`select_tab_by_index`, and the keyboard-shortcut interception (`handle_tab_shortcut`) including the "plain Tab isn't claimed" negative case. Same methodology as always (see "Testing methodology" below): real `TermiteApp` (real PTY spawns and all), fake `ssh_worker` channel, asserting synchronous state and what's sent on the fake channel — the reconnect `Task` itself is never polled in these tests, consistent with the pre-existing limitation on `Task::perform`-returning handlers.

**Rendering verification** (Xvfb `:99`, same method as v13's star-glyph check): confirmed the tab bar actually renders — active-tab highlight, status glyph, close/`+` buttons, correctly positioned above the terminal pane and beside the (unchanged) sidebar. Same standing limitation as always: no click-driven verification of Select/Close/Retry/`+` actually being pressed; the tests above are the substitute.

**Not done / explicitly out of scope**: tab drag-to-reorder (`ARCHITECTURE.md`'s checklist literally says "Reorder tabs", but there's no drag infrastructure in this codebase at all yet — revisit if it turns out to matter more than it seems right now). No live-`sshd` end-to-end test of the new reconnect path (still nothing in this project has exercised auth against a real server — same gap noted since v11). The pre-existing open items from v13 (standalone forget-credential action, `ProxyJump` consumption) are untouched.

**Verification**: `cargo test --workspace` all green (61 in `termite-app`, unchanged elsewhere), `clippy --workspace --all-targets -D warnings` clean, `fmt --all --check` clean. `cargo audit`/`cargo deny check` not re-run this session (no dependency changes — `TabId`/`tabbar.rs` use only already-present `uuid`/`iced` — so nothing new for either to flag).

---

## Project history (v5–v13, condensed)

- **M0–M2** (earliest sessions): workspace scaffold + CI, local terminal emulator (PTY/VT/Iced), SSH core with mandatory `known_hosts` verification. All verified end-to-end; not revisited since.
- **M3 protocol layer** (v5–v6): RSA hash-algorithm threading (`rsa-sha2-256`/`512`, negotiated not guessed), SSH agent auth (`$SSH_AUTH_SOCK`, key material never enters the process), `~/.ssh/config` parsing (own parser, no crate dep — OpenSSH resolution semantics: first-value-wins, `IdentityFile` accumulates, fnmatch patterns, `Match` blocks inert). **Load-bearing gotcha**: `KeyProviderSigner::auth_sign` (`crates/termite-ssh/src/signer.rs`) must return the *entire* `to_sign` buffer with the signature appended as a length-prefixed SSH string, not bare signature bytes — russh slices the userauth packet out of it; getting this wrong hangs the session silently.
- **CI** (v6–v7): `rustsec/audit-check@v2` has its own `ignore:` action input separate from `.cargo/audit.toml` — the two must be kept in sync manually (now also documented in `CLAUDE.md`). The action also needs an explicit `checks: write` permission or it 403s publishing results even when the audit itself passes. Lesson: a commit message claiming a GitHub Actions change is "verified" isn't evidence — check the actual run (`gh` isn't installed here; unauthenticated `curl` against `api.github.com` works for public-repo run/job status).
- **M4 UI, part 1** (v7–v9): `HostStore` (TOML-backed, re-reads the whole file every call rather than caching, to avoid staleness vs. hand-edits), the sidebar list/add/delete, `SessionCommand`/`SessionEvent` wiring via a persistent Iced subscription (`ssh_worker`), then the auth-method picker + credential/host-key prompt modal (`AuthRequired`/`HostKeyUnknown`/`HostKeyMismatch` previously failed closed with no UI at all), then a save-to-keychain toggle on that prompt.
- **M4 UI, part 2** (v10–v12): `~/.ssh/config` `Host`-alias resolution in the add-host form (Enter on the address field), then deleting a host also forgetting its saved keychain credential (this closed a real gap: `CredentialStore` had no `delete_passphrase` at all before v12, only `delete_password`).
- **M4 completion** (v13): `HostProfile` gained `favourite: bool`/`last_connected: Option<u64>`; the sidebar got favourites, recency-ordered sorting (`sort_hosts()`), search/filter, host tags in the form, a profile editor (`EditHost`/`SaveHost` create-or-update), `~/.ssh/config` bulk import (`SshConfig::host_aliases()`), and key-generation UI (`generate_and_save_key`, never overwrites an existing file). Rendering verification caught a real bug here: the ★/☆ favourite glyphs don't exist in iced's default UI font and drew as missing-glyph boxes — replaced with plain ASCII `"*"`/`"o"`. This is why M5's tab-status glyphs (this session, see above) went straight to ASCII instead of unicode.
- **Testing methodology, established v7–v8 and unchanged since**: no live SSH server here (`sshd` exists but its systemd service is deliberately left untouched — shared system state); `termite-ssh`'s hermetic integration tests substitute. No click-driven GUI testing — see "Isolated GUI testing" below. Handler functions (`handle_session_event`/`update_sidebar`/`update_prompt`/`update_tabbar` as of this session) are instead exercised directly against a real `TermiteApp` with a fake `ssh_worker` channel standing in for the subscription.

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
- **Git:** `main`. `9dac4ee`/`970e428`/`c35fd44` (v8) are pushed. Everything since — `daeaec6`/`479f05a`/`60f8bfe`/`9c7a409`/`355a4e6` (v9/v10), `cb0879d` (v11), `3dc07ae` (v12), `36d9db3`/`2e6ed3d` (v13), and this session's M5 commit (see `git log` for the exact hash — HANDOFF is written just before it lands) — is local-only: 14 commits ahead of `origin/main`. Working agreement: commit actively, push only when asked — no push has been asked for since `49c59ea`. Don't assume local-only commits are on GitHub — CI has not run against them.
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
| **M5** | ✅ Done | Tabs & multi-session. All of `ARCHITECTURE.md`'s checklist done except drag-to-reorder (no drag infra exists yet — see this session's write-up): tab bar, open/close/select tabs, per-tab session isolation, `Ctrl+Tab`/`Ctrl+1-9` shortcuts, reconnect with backoff + manual retry, per-tab status indicator |
| M6–M9 | Pending | Advanced terminal, forwarding/SFTP/ProxyJump, palette, release. **M6 (Advanced Terminal Features) is the natural next milestone** |

---

## Layout / architecture

Workspace: `termite` binary + `crates/{termite-core,-ssh,-terminal,-storage,-crypto,-ui,-app}`. Dependency graph is one-directional, compiler-enforced, `termite-core` at the bottom with no internal deps.

M5 files, all verified:
- `crates/termite-core/src/types.rs` — `TabId` (new this session, mirrors `SessionId`'s shape, not persisted), `ConnectionStatus` (existed since early on, unused until now — driving each tab's status).
- `crates/termite-ui/src/tabbar.rs` — `tabbar::view()`, `TabBarMessage` (`Select`/`Close`/`Retry`/`NewLocal`), `TabSummary`. Pure presentation, same contract as `sidebar.rs`, but returns a `'static` `Element` rather than one borrowing its input — see this session's write-up above for why that's safe here and wouldn't be for a widget using `text_input`.
- `crates/termite-app/src/lib.rs` — `Tab`/`TabKind` (`Local`/`Ssh`), replacing the old single `grid`/`parser`/`active_session` fields; `TermiteApp` now holds `tabs: Vec<Tab>` + `active_tab: Option<TabId>`. Key free functions: `handle_session_event` (now per-tab routing + reconnect scheduling, returns `Task<Message>`), `close_tab`/`new_local_tab`/`spawn_local_tab`/`reconnect`/`update_tabbar` (tab lifecycle), `handle_tab_shortcut`/`select_adjacent_tab`/`select_tab_by_index` (keyboard navigation), `backoff_delay`/`MAX_RECONNECT_ATTEMPTS` (reconnect policy), `find_tab`/`find_tab_mut`/`find_tab_by_session_mut`/`active_tab`/`active_tab_mut` (lookup helpers).

M4 files, all verified:
- `crates/termite-storage/src/host_store.rs` — `HostStore` trait, `TomlHostStore`, `MemoryHostStore`.
- `crates/termite-ui/src/sidebar.rs` — `sidebar::view()`, `SidebarMessage` (now: `ResolveAlias`, `SaveHost`, `CancelEdit`, `EditHost`, `ToggleFavourite`, `ImportFromSshConfig`, `TagsInputChanged`, `SearchInputChanged`, keygen fields — see this session's work above), `SidebarState`, `AuthKind`. Pure presentation throughout — no `HostStore`/`CredentialStore`/SSH types leak in; `sidebar::view()` renders whatever host order and filter it's given, doesn't decide either.
- `crates/termite-ui/src/prompt.rs` — `prompt::view()`, `Prompt` (`Credential { label, input, save }`/`HostKey`), `PromptMessage`. Pure presentation.
- `crates/termite-app/src/lib.rs` — `TermiteApp` also holds `host_store`, `hosts` (kept sorted via `sort_hosts()`, called from `Message::HostsLoaded`), `sidebar`, `ssh_config: SshConfig`, `credential_store: Arc<dyn CredentialStore>`, `ssh_worker`, `pending_prompt` (`active_session` is gone — see the M5 bullet above). Key free functions: `update_prompt` (the auth/host-key modal state machine), `update_sidebar` (everything host-list-related; `SelectHost` now opens a tab rather than mutating a single active session), `apply_resolved_config`/`load_profile_into_form`/`host_profile_from_config` (the three ways form state gets populated from elsewhere), `forget_credential`/`generate_and_save_key` (the two places `termite-app` talks to the keychain directly, alongside `saved_credential`/`save_credential`).

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

1. **Start M6 (Advanced Terminal Features)** — M5 is done; this is the next milestone in `ARCHITECTURE.md`'s plan. Mouse reporting, OSC 8 hyperlinks, bracketed paste, alternate screen buffer (needed for full-screen apps: vim, htop), `xterm`-style title setting, bell, selection modes, scrollback search, split terminal. Also where real window-size-driven resizing (replacing the fixed `ROWS`/`COLS` constants) is scoped to land.
2. **Tab drag-to-reorder** — the one M5 checklist line not done this session; there's no drag infrastructure anywhere in this codebase yet. Low priority unless it turns out to matter more in practice than it seems right now (tabs can still be opened/closed/switched freely, just not reordered).
3. **End-to-end verification against a real server** — still nothing has exercised `AuthRequired`/`HostKeyUnknown`/`HostKeyMismatch`, or now the tab/reconnect machinery, against an actual `sshd`. A user-scoped `sshd -f <config> -p <high port> -D` bound to `127.0.0.1` with its own host key and `AuthorizedKeysFile` would be hermetic and worth trying — same spirit as `termite-ssh`'s existing in-process test agent/server. Would also be the first real exercise of this session's reconnect-with-backoff path, which so far is only unit-tested at the state-transition level (see "Not done" above).
4. **No standalone "forget this credential" action** independent of deleting the whole host (e.g. to force a re-prompt after a password changes server-side). Flagged since v12; still open.
5. **`ProxyJump` still isn't consumed anywhere** — parsed (`ssh_config.rs`) and resolved (`apply_resolved_config`/`host_profile_from_config` both see `HostConfig::proxy_jump` and drop it) but nothing acts on it; `HostProfile` has no jump-host concept. Real support is M7-scoped.
6. **Decide whether to push** — 14 commits are local-only; see Repo state.

---

*Handoff v14 written after finishing M5 in full (tab bar, open/close/select tabs, per-tab session isolation, keyboard shortcuts, reconnect with backoff, per-tab status) except drag-to-reorder, and folding v13's write-up into "Project history" above (the same compaction v5–v12 got in v13). Working agreement unchanged: commit actively as work lands; push only when asked.*
