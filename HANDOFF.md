# Termite — Conversation Handoff (v3)

This document gives the next conversation full context to continue development without losing anything. It supersedes v2. **Read this one, not v2 or conversation 1's — this one records M1 as actually, verifiably done.**

---

## M1 is done — verified, not just claimed

Two prior handoffs (and one commit message, `aa8d928`) made claims about M1 status that didn't hold up. This time it was actually run: `cargo build --workspace` succeeded, `cargo run` opened a real window, a real `bash` shell spawned (its rc file ran — fastfetch output appeared, correctly parsed and rendered), and a human typed `echo hello` into the window and it echoed and executed correctly. Both the output path (PTY read → `vte::Parser` → `GridHandler` → `TerminalGrid` → text render) and the input path (keypress → VT bytes → PTY write) are confirmed working end-to-end, not just compiling.

**Trust `git show --stat <commit>` over commit messages when auditing this repo's history** — that habit is still warranted even though this handoff's own claims are verified.

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
- **Git:** `main` branch, pushed and up to date with `origin/main` as of session start. This session added two commits on top of `be48398`:
  - `bc0813a` — `feat(terminal): implement GridHandler and Pty for M1` (handler.rs, pty.rs, lib.rs wiring)
  - `1dee84b` — `feat(app): wire local PTY terminal into termite-app for M1` (termite-app/src/lib.rs, Cargo.toml, Cargo.lock)
- **Working tree:** clean as of end of session (verify with `git status` — don't assume, this repo's history has a track record of stale claims).
- Not yet pushed to `origin` — the two new commits are local only. Push if/when asked.

---

## Workspace structure

```
termite/
├── Cargo.toml                  # Workspace + binary package
├── src/main.rs                 # Entry: calls termite_app::run()
├── ARCHITECTURE.md             # Full design doc — read this
├── CLAUDE.md                   # Command reference, security invariants, kept current
├── CONTRIBUTING.md
├── SECURITY.md
├── LICENSE (MIT)
├── README.md
├── .gitignore / .claudeignore
├── deny.toml                   # cargo-deny: licence allow-list, openssl banned
├── .github/workflows/ci.yml    # lint + test (Linux/macOS/Win) + audit + deny
└── crates/
    ├── termite-core/           # Shared types: SessionId, HostProfile, AuthMethod,
    │                           # ConnectionStatus, TermiteError. NO other workspace deps.
    ├── termite-ssh/            # STUB — implemented in M2
    ├── termite-terminal/       # DONE (M1) — see below
    ├── termite-storage/        # STUB — implemented in M3
    ├── termite-crypto/         # STUB — implemented in M3
    ├── termite-ui/             # Theme + colour palette. TermiteTheme type.
    └── termite-app/            # Iced 0.13 app. M1 terminal wired in (see below).
```

---

## M1 (terminal emulator) — final state

### `crates/termite-terminal/`
- **`cell.rs`** — `Cell { ch, fg, bg, attrs }`, `TermColor` (Default/Indexed(u8)/Rgb(u8,u8,u8)), `Attrs` (bold/dim/italic/underline/reverse/strikethrough).
- **`grid.rs`** — `TerminalGrid`: primary + alt screen buffers, cursor state + save/restore, `put_char`/`linefeed`/`carriage_return`/`backspace`, scrollback (`VecDeque`, capped 10,000 lines), cursor movement, `erase_line`/`erase_display` with `EraseMode`, pending SGR state (`reset_sgr`/`set_fg`/`set_bg`/`set_attrs`), `resize`, `visible_rows() -> Vec<String>`, title.
- **`handler.rs`** — `GridHandler<'a> { pub grid: &'a mut TerminalGrid }` implementing `vte::Perform`:
  - `print`/`execute` (LF/CR/BS) → direct grid calls.
  - `csi_dispatch` — cursor movement (A/B/C/D, with the VT convention that a `0`/absent param means move-by-1), `H`/`f` cursor position, `J`/`K` erase (via `EraseMode::from_param`), `m` SGR, and DEC private mode `?1049h`/`?1049l` (alt screen) gated on `intermediates == b"?"`.
  - SGR parsing flattens `Params` (which groups colon-subparams into one slice per parameter, so semicolon-separated `38;2;r;g;b` arrives as four separate length-1 slices) into a single `Vec<u16>`, then walks it with a manual index so the `38`/`48` extended-colour forms (`;5;n` indexed, `;2;r;g;b` truecolour) can consume the right number of trailing elements.
  - `esc_dispatch` — `ESC 7`/`8` save/restore cursor.
  - `osc_dispatch` — `0`/`2` set title.
- **`pty.rs`** — `Pty::spawn(shell, rows, cols) -> Result<Pty, PtyError>` via `portable-pty` 0.8.1's real API (verified against cached source, not guessed): `native_pty_system().openpty(PtySize)`, `pair.slave.spawn_command(cmd)`, explicit `drop(pair.slave)` after spawn (both `master`/`slave` fields on `PtyPair`, no `Copy`, so this is a partial-move-then-drop-remaining-field pattern) to avoid the child's fds staying held open in this process. `try_clone_reader`/`take_writer`/`resize` all take `&self` per the real trait (not `&mut self` as loosely assumed in v2's unverified sketch), so `Pty` exposes them as `&self` methods too. `PtyError` is a `thiserror` enum; `portable_pty`'s own error type is `anyhow::Error` (not part of its public API surface — `use anyhow::Error` internally, not `pub use`), so errors are converted with `.to_string()` at the boundary rather than named directly.
- **`lib.rs`** — `pub mod cell/grid/handler/pty;` plus re-exports (`Cell`, `TermColor`, `Attrs`, `TerminalGrid`, `EraseMode`, `GridHandler`, `Pty`, `PtyError`).

### `crates/termite-app/src/lib.rs`
- `TermiteApp` (no longer the M0 unit struct): holds `grid: TerminalGrid`, `parser: vte::Parser`, `output: Arc<Mutex<Vec<u8>>>`, `writer: Box<dyn Write + Send>`, and `_pty: Pty` (kept alive; unused directly until session lifecycle work in M2+ needs it).
- `TermiteApp::new()` spawns `$SHELL` (falls back to `/bin/bash`) at a fixed 30×100 grid, clones a PTY reader onto a `std::thread` that pushes bytes into the shared `output` buffer, and takes the writer for input.
- Built via `iced::application(...).subscription(subscription).run_with(initialize)` — `run_with` (not `run`) because `TermiteApp` doesn't implement `Default` (it needs fallible PTY-spawn initialization).
- `Message` is `PollOutput` and `KeyPressed { key: Key, modifiers: Modifiers }`.
- Subscription batches `iced::time::every(16ms).map(|_| Message::PollOutput)` and `iced::keyboard::on_key_press(|key, modifiers| Some(Message::KeyPressed { key, modifiers }))` — confirmed `on_key_press` requires a plain `fn` pointer (non-capturing closures coerce fine, which is what's used).
- `update` on `PollOutput` drains the shared buffer (`std::mem::take`) and feeds it through the grid/parser split-borrow pattern from v2's handoff (parser and grid taken as separate mutable field borrows so the parser can hold a `&mut GridHandler` borrowing grid without conflicting with `&mut self.parser`). On `KeyPressed`, maps the key to VT bytes and writes them to the PTY.
- `view` renders `grid.visible_rows().join("\n")` as one `text(...)` widget with `Font::MONOSPACE`, size 14. No colour/attribute rendering yet — deferred to M6 as originally planned; the grid already tracks the data.
- `key_to_bytes` covers Enter/Backspace/Tab/Escape/arrows/Space/plain characters, and `Ctrl+<alpha>` (mapped to the corresponding C0 control byte).

### Verified this session
- `cargo check --workspace`, `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` — all clean, zero warnings.
- `cargo fmt --all --check` — clean for every file touched this session. It reports diffs in two **pre-existing, untouched** files (`crates/termite-core/src/types.rs`, `crates/termite-ui/src/theme.rs`) that use manual column-aligned struct fields from earlier milestones — not introduced or fixed this session, left alone since fixing unrelated files wasn't requested. Worth a note to the user if `cargo fmt --all --check` is relied on as a CI gate — it currently fails on `main` because of those two files, independent of anything M1 touched.
- Manual launch (`cargo run`, screenshotted while running): real window opened, real bash session started including shell rc execution, output rendered correctly as monospaced text, and a human-typed `echo hello` echoed and executed. Both PTY output and PTY input paths confirmed live, not just unit-tested.

### What M1 does NOT do (deferred, as originally scoped)
- Mouse reporting (M6).
- Coloured/attributed rendering in the Iced view — the grid tracks fg/bg/attrs per cell already, just not wired into `view()` yet.
- Wide character support (CJK/emoji) — treated as width 1.
- Scrollback UI, split panes (M6).
- Dynamic grid resize on window resize — fixed 30×100 for now; `TerminalGrid::resize`/`Pty::resize` both exist and work, just nothing calls them yet.
- Windows ConPTY testing (M4) — this session's manual verification was Linux/X11 only.

---

## Key architectural decisions (already locked in)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| UI framework | **Iced 0.13** | Pure Rust, GPU (wgpu), TEA architecture, MIT |
| SSH library | **russh** | Pure Rust SSH-2, no FFI, MIT |
| VT parsing | **vte 0.13** | Used by Alacritty, Apache-2.0 |
| PTY | **portable-pty 0.8** | Cross-platform, MIT, from WezTerm |
| Async runtime | **tokio 1** | Standard |
| Key formats | **ssh-key 0.6** | RustCrypto, pure Rust |
| Credential storage | **keyring 2** | OS keychain: macOS/Win/Linux |
| Secret memory | **secrecy 0.10 + zeroize 1** | Mandatory for all secret types |
| Config format | **toml 0.8 + serde 1** | Human-editable |
| Error handling | **thiserror 2** (libs) + **anyhow 1** (app) | Standard |
| Logging | **tracing 0.1** | Structured, async-aware |

See ARCHITECTURE.md §4 for full crate justifications.

---

## Dependency graph between crates

```
termite-app
  ├── termite-ui      → termite-core, iced
  ├── termite-ssh     → termite-core, termite-crypto
  ├── termite-terminal → termite-core
  ├── termite-storage → termite-core
  └── termite-crypto  → termite-core
termite-core          → (no workspace deps)
```

This is enforced at the compiler level. Nothing leaks across layers.

---

## Security invariants (never break these)

1. Secrets (`passwords`, `key material`) are ALWAYS wrapped in `secrecy::SecretString` or `secrecy::SecretVec<u8>` — never plain `String` or `Vec<u8>`.
2. `zeroize` zeroes memory on drop. All secret types implement `ZeroizeOnDrop`.
3. Host key verification is MANDATORY. Changed keys produce a prominent warning, never silent accept.
4. Passwords and key passphrases are stored in the OS keychain (`keyring`), NEVER in config files on disk.
5. Secrets NEVER appear in `tracing` log output. `secrecy` enforces this via its `Debug` impl (`[REDACTED]`).

(None of this is touched by M1 — no secrets flow through the terminal emulator.)

---

## Commit conventions

Conventional Commits format: `<type>(<scope>): <description>`

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `security`
Scopes: `core`, `ssh`, `terminal`, `storage`, `crypto`, `ui`, `app`

This session's commits followed this and can be used as a reference:
```
feat(terminal): implement GridHandler and Pty for M1
feat(app): wire local PTY terminal into termite-app for M1
```

Git worked normally this session — plain `git add`/`git commit`, no lock-file workaround needed. The `git commit` sandbox workaround documented in older handoffs appears to be stale/environment-specific; no need to carry it forward unless it actually recurs.

---

## Roadmap — milestone status

| Milestone | Status | Description |
|-----------|--------|-------------|
| **M0** | ✅ Done | Workspace scaffold, CI, window opens |
| **M1** | ✅ Done | Local terminal emulator (PTY + VT emulation + Iced rendering) — verified end-to-end this session |
| M2 | Pending | SSH core (password auth, known hosts) |
| M3 | Pending | Key auth + credential storage |
| M4 | Pending | Host management UI |
| M5 | Pending | Tabs + multi-session |
| M6 | Pending | Advanced terminal (colours, mouse, split, scrollback UI, dynamic resize) |
| M7 | Pending | Port forwarding, SFTP |
| M8 | Pending | Command palette, UX polish |
| M9 | Pending | Security review, packaging, public release |

---

## Suggested next steps (M2 — SSH core)

Not started. `termite-ssh` is still a stub. Per ARCHITECTURE.md, M2 scope is password auth + known_hosts verification over `russh`, with the session communicating to `termite-app` via `tokio::sync::mpsc` (`SessionEvent` out, `SessionCommand` in), bridged into Iced via a `Subscription` — mirroring the pattern this session used for PTY output (background task/thread → shared state → polled or bridged into a message), but with `SessionEvent::HostKeyUnknown`/`HostKeyMismatch` handled explicitly per the security invariants above (no silent accept, ever). Read ARCHITECTURE.md §6-8 before starting.

Also worth doing opportunistically, not blocking M2: fix the two pre-existing `cargo fmt` violations in `termite-core/src/types.rs` and `termite-ui/src/theme.rs` (see "Verified this session" above) so `cargo fmt --all --check` actually passes on `main` again, since CI runs it.

---

*Handoff v3 written after completing and manually verifying M1, this conversation. Continue with M2 (SSH core) next.*
