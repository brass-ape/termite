# Termite — Conversation Handoff

This document gives the next conversation full context to continue development without losing anything.

---

## What this project is

**Termite** — a modern, open-source, native SSH client built entirely in Rust.
- No accounts, no telemetry, no subscriptions, no AI, no cloud.
- Target aesthetic: VS Code / Warp / Obsidian — not PuTTY.
- Cross-platform: Linux, macOS, Windows.
- Philosophy: "It should disappear and let you work."

Full design rationale is in `ARCHITECTURE.md`. Read that first.

---

## Repo state

- **Location:** `~/Documents/code/termite`
- **Git:** Initialised, `main` branch, 2 commits locally.
  - `e8b6dca` — docs: initial project documentation and architecture
  - `5ff219a` — chore(m0): scaffold Cargo workspace and crate skeletons
- **Remote:** `https://github.com/brass-ape/termite.git` — needs `git push origin main` (sandbox proxy blocks push; user must push manually).

---

## Workspace structure

```
termite/
├── Cargo.toml                  # Workspace + binary package
├── src/main.rs                 # Entry: calls termite_app::run()
├── ARCHITECTURE.md             # Full design doc — read this
├── CONTRIBUTING.md
├── SECURITY.md
├── LICENSE (MIT)
├── README.md
├── .gitignore
├── deny.toml                   # cargo-deny: licence allow-list, openssl banned
├── .github/workflows/ci.yml    # lint + test (Linux/macOS/Win) + audit + deny
└── crates/
    ├── termite-core/           # Shared types: SessionId, HostProfile, AuthMethod,
    │                           # ConnectionStatus, TermiteError. NO other workspace deps.
    ├── termite-ssh/            # STUB — implemented in M2
    ├── termite-terminal/       # STUB — implement next (M1)
    ├── termite-storage/        # STUB — implemented in M3
    ├── termite-crypto/         # STUB — implemented in M3
    ├── termite-ui/             # Theme + colour palette. TermiteTheme type.
    └── termite-app/            # Iced 0.13 app skeleton. Dark theme. Tracing init.
```

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

---

## Commit conventions

Conventional Commits format: `<type>(<scope>): <description>`

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `security`
Scopes: `core`, `ssh`, `terminal`, `storage`, `crypto`, `ui`, `app`

Examples:
```
feat(terminal): implement ANSI 256-colour support
fix(ssh): handle keepalive timeout during reconnect
security(crypto): zeroize key material on parse error
```

### Git commit workaround (IMPORTANT)

The sandbox filesystem blocks `unlink` on git lock files. Normal `git commit` will fail. Use this pattern for every commit:

```bash
cd /sessions/admiring-relaxed-gates/mnt/termite   # bash path for the workspace

# Stage files using /tmp index (no mount restrictions)
GIT_INDEX_FILE=/tmp/termite-index git add -A

# Write tree and create commit using plumbing (bypasses index.lock)
TREE=$(GIT_INDEX_FILE=/tmp/termite-index git write-tree)
PARENT=$(git rev-parse HEAD)
COMMIT=$(git commit-tree "$TREE" -p "$PARENT" -m "your message here")
echo "$COMMIT" > .git/refs/heads/main

# Verify
git log --oneline
```

Push requires the user to run `git push origin main` from their terminal — sandbox proxy blocks outbound HTTPS to GitHub.

---

## Roadmap — milestone status

| Milestone | Status | Description |
|-----------|--------|-------------|
| **M0** | ✅ Done | Workspace scaffold, CI, window opens |
| **M1** | 🔨 Next | Local terminal emulator (PTY + VT emulation + Iced rendering) |
| M2 | Pending | SSH core (password auth, known hosts) |
| M3 | Pending | Key auth + credential storage |
| M4 | Pending | Host management UI |
| M5 | Pending | Tabs + multi-session |
| M6 | Pending | Advanced terminal (colours, mouse, split) |
| M7 | Pending | Port forwarding, SFTP |
| M8 | Pending | Command palette, UX polish |
| M9 | Pending | Security review, packaging, public release |

---

## M1 — What to build next

M1 delivers a working terminal emulator attached to a local shell (no SSH yet). When done, the app opens and shows a functional interactive shell session.

### Files to create/modify for M1

**`crates/termite-terminal/Cargo.toml`** — add:
```toml
vte          = "0.13"
portable-pty = "0.8"
thiserror    = { workspace = true }
tracing      = { workspace = true }
```

**New source files in `crates/termite-terminal/src/`:**
- `cell.rs` — `Cell { ch, fg, bg, attrs }`, `TermColor` (Default/Indexed/Rgb), `Attrs` (bold/italic/underline/etc.)
- `grid.rs` — `TerminalGrid`: 2D Vec<Cell>, cursor, scroll region, scrollback, alt screen, SGR state
- `handler.rs` — `GridHandler` implementing `vte::Perform` — routes escape sequences to grid operations
- `pty.rs` — `Pty::spawn(shell, rows, cols)` using `portable-pty`; returns reader + writer

**Updated `crates/termite-app/src/lib.rs`** — integrate the terminal:
- `TermiteApp` state: `TerminalGrid`, `vte::Parser`, `OutputBuffer` (Arc<Mutex<VecDeque<Vec<u8>>>>), PTY writer
- On init: spawn PTY (default shell via `$SHELL`), start std::thread to read PTY → push to OutputBuffer
- Subscription: `iced::time::every(16ms)` to poll OutputBuffer + `iced::keyboard::on_key_press`
- `Message::PollOutput` — drain buffer, feed bytes to `vte::Parser` → updates grid
- `Message::KeyPressed { key, modifiers }` — convert to VT byte sequence, write to PTY writer
- View: iterate grid rows, render each as `text(row_string).font(Font::MONOSPACE).size(14)`

### Key implementation notes for M1

**Borrow checker pattern for vte parsing** (grid and parser are separate fields):
```rust
pub fn advance(&mut self, bytes: &[u8]) {
    let parser = &mut self.parser;   // split borrow
    let grid   = &mut self.grid;     // split borrow — Rust allows this for separate fields
    let mut handler = GridHandler { grid };
    for &byte in bytes {
        parser.advance(&mut handler, byte);
    }
}
```

**PTY output polling pattern** (avoids complex Iced subscription wiring):
```rust
// std::thread (not tokio) reads PTY → pushes to shared buffer
// Iced timer subscription drains buffer every 16ms
fn subscription(app: &TermiteApp) -> iced::Subscription<Message> {
    use std::time::Duration;
    iced::Subscription::batch([
        iced::time::every(Duration::from_millis(16)).map(|_| Message::PollOutput),
        iced::keyboard::on_key_press(|key, mods| Some(Message::KeyPressed { key, mods })),
    ])
}
```

**Shell detection:**
```rust
let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
// Windows: "cmd.exe" — handle in M4 with proper platform detection
```

**VT sequences the handler MUST implement for basic usability:**
- `print(c)` → `grid.put_char(c)`
- `execute(0x0a)` LF → `grid.linefeed()`
- `execute(0x0d)` CR → `grid.carriage_return()`
- `execute(0x08)` BS → `grid.backspace()`
- CSI A/B/C/D — cursor up/down/forward/back
- CSI H/f — cursor position (row;col)
- CSI J — erase display (0/1/2)
- CSI K — erase line (0/1/2)
- CSI m — SGR (colors + bold/italic/underline/reverse)
- CSI ?1049h/l — enter/leave alternate screen
- ESC 7/8 — save/restore cursor
- OSC 0/2 — set terminal title

**SGR color mapping (CSI m):**
- 0 → reset all
- 1/2/3/4/7/9 → bold/dim/italic/underline/reverse/strikethrough
- 30-37 → fg Indexed(0-7), 90-97 → fg Indexed(8-15) (bright)
- 40-47 → bg Indexed(0-7), 100-107 → bg Indexed(8-15)
- 38;5;n → fg Indexed(n) (256-color)
- 38;2;r;g;b → fg Rgb(r,g,b) (true color)
- 39 → fg Default, 49 → bg Default

**Key-to-VT mapping (essential keys):**
```
Enter → \r
Backspace → \x7f
Tab → \t
Escape → \x1b
ArrowUp → \x1b[A
ArrowDown → \x1b[B
ArrowRight → \x1b[C
ArrowLeft → \x1b[D
Ctrl+<alpha> → byte (char - 'a' + 1)
```

### What M1 does NOT need to do (deferred)
- Mouse reporting (M6)
- True colour in Iced renderer — just render text as monochrome for M1, colours in M6
- Wide character support (CJK/emoji) — treat all chars as width 1 for M1
- Scrollback UI (M6)
- Split panes (M6)
- Windows ConPTY testing (M4)

---

## Working in this codebase

```sh
# Check (fast — no linking)
cargo check --workspace

# Build
cargo build

# Test
cargo test --workspace

# Lint
cargo clippy --workspace -- -D warnings

# Dependency audit
cargo audit

# Format
cargo fmt
```

CI runs all of the above on push to `main` across Linux, macOS, and Windows.

---

*Handoff written at end of conversation 1. Continue from M1.*
