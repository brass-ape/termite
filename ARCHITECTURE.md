# Termite — Architecture Document

> "It should disappear and let you work."

This document is the authoritative reference for Termite's design decisions, module structure, crate choices, and inter-module communication. It is a living document and should be updated whenever a significant architectural decision changes.

---

## Table of Contents

1. [Project Overview](#1-project-overview)
2. [The Big Decision: UI Framework](#2-the-big-decision-ui-framework)
3. [Workspace Structure](#3-workspace-structure)
4. [Crate Inventory & Justification](#4-crate-inventory--justification)
5. [Module Responsibilities](#5-module-responsibilities)
6. [Inter-module Communication](#6-inter-module-communication)
7. [Terminal Emulation Architecture](#7-terminal-emulation-architecture)
8. [SSH Layer Architecture](#8-ssh-layer-architecture)
9. [Security Architecture](#9-security-architecture)
10. [Credential & Key Storage](#10-credential--key-storage)
11. [State Management](#11-state-management)
12. [Performance Considerations](#12-performance-considerations)
13. [Testing Strategy](#13-testing-strategy)
14. [Licensing](#14-licensing)
15. [Development Roadmap](#15-development-roadmap)

---

## 1. Project Overview

Termite is a native, cross-platform SSH client written entirely in Rust. It targets Linux, macOS, and Windows. It has no accounts, no telemetry, no subscriptions, and no cloud connectivity of any kind. Everything runs and stays on the user's machine.

The intended aesthetic is closer to VS Code, Obsidian, or Warp than to PuTTY. The user should never think about the tool — only the work.

**Design priorities, in order:**
1. Security — never compromise
2. Correctness — bugs are worse than missing features
3. Performance — the UI must feel instant
4. Simplicity — add features deliberately, remove accidental complexity
5. Aesthetics — polish matters for a tool people use every day

---

## 2. The Big Decision: UI Framework

This is the most consequential choice in the project. Get it wrong and you either rebuild the UI layer or accept permanent limitations.

### Options evaluated

| Framework | Model | Rendering | Rust-native | Maturity | License |
|-----------|-------|-----------|-------------|----------|---------|
| **Iced** | Retained (TEA) | wgpu (GPU) | Yes | Moderate | MIT |
| egui | Immediate | wgpu/OpenGL | Yes | High | MIT |
| Tauri | Retained | Webview | Partial | High | MIT |
| Slint | Declarative | Software/GPU | Yes | Moderate | GPL-3 / Commercial |
| GPUI (Zed) | Retained | Metal/wgpu | Yes | High | Apache-2.0 |

### Decision: Iced

**Why Iced:**

- **Pure Rust end-to-end.** No JavaScript engine, no webview, no C bindings in the UI layer. The entire stack is auditable.
- **GPU-accelerated.** Iced uses `wgpu`, which targets Vulkan, Metal, DirectX 12, and WebGPU. It will be fast everywhere.
- **Retained mode with TEA (The Elm Architecture).** For a complex application with multiple sessions, tabs, split panes, and async state, retained mode scales. Immediate mode (egui) would require increasingly heroic state management.
- **cosmic-text integration.** Iced's text backend uses cosmic-text, which provides excellent Unicode, emoji, bidirectional text, and subpixel rendering. Typography quality is a product differentiator.
- **MIT license.** Clean. No GPL contamination concerns for the project.
- **Principled architecture.** The TEA model (Model → View → Message → Update) produces testable, predictable UI logic. State changes are explicit.

**Why not Tauri:** Tauri produces beautiful UIs but the frontend is a webview. That means shipping a browser engine, dealing with JS/Rust bridging, and the styling is CSS rather than Rust. For an open-source project where "everything is Rust" is a meaningful property, Tauri muddies the story. It is also harder to build a high-performance terminal renderer in a webview.

**Why not egui:** Excellent for developer tools and immediate prototyping. Harder to achieve smooth animations, sophisticated layout, and premium typography. The immediate mode model also makes it harder to represent complex async state cleanly.

**Why not Slint:** GPL-3 for open-source use. That restricts downstream licensing options and would make the project harder to embed or build upon. Commercial license has cost. This is a dealbreaker.

**Why not GPUI:** GPUI is Zed's internal framework and is not designed as a general-purpose library. API stability is not guaranteed for external consumers.

### Escape hatch

If Iced's widget ecosystem proves insufficient as the project matures (particularly for the split terminal view or SFTP file browser), the business logic (SSH, terminal emulation, storage, crypto) is completely decoupled from the UI layer. Migrating to a different UI framework would mean rewriting `termite-ui` and `termite-app`, not the entire project. Design for this escape hatch from day one: no UI types should leak into `termite-ssh`, `termite-terminal`, or `termite-storage`.

---

## 3. Workspace Structure

Termite uses a Cargo workspace. Each logical concern is a separate crate. This enforces separation of concerns at the compiler level — if `termite-ssh` tries to import an Iced widget, it will not compile.

```
termite/
├── Cargo.toml                  # Workspace manifest
├── Cargo.lock                  # Committed (binary crate)
├── ARCHITECTURE.md             # This document
├── ROADMAP.md
├── README.md
├── CONTRIBUTING.md
├── SECURITY.md
├── LICENSE                     # MIT
│
├── .github/
│   └── workflows/
│       ├── ci.yml              # Lint, test, build on Linux/macOS/Windows
│       ├── release.yml         # Build installers on tag push
│       └── audit.yml           # cargo-audit on schedule
│
├── crates/
│   ├── termite-core/           # Shared types, errors, traits — no dependencies on other crates
│   ├── termite-ssh/            # SSH protocol, session lifecycle
│   ├── termite-terminal/       # VT emulation, PTY management, grid state
│   ├── termite-storage/        # Config files, host profiles, settings
│   ├── termite-crypto/         # Key loading/generation, secure wrappers
│   ├── termite-ui/             # Iced widgets, theming, color palette
│   └── termite-app/            # Top-level app state, message routing
│
└── src/
    └── main.rs                 # Thin entry point — calls termite_app::run()
```

### Dependency graph between crates

```
main.rs
  └── termite-app
        ├── termite-ui
        │     └── termite-core
        ├── termite-ssh
        │     ├── termite-core
        │     └── termite-crypto
        │           └── termite-core
        ├── termite-terminal
        │     └── termite-core
        └── termite-storage
              └── termite-core
```

`termite-core` depends on nothing in this workspace. It is the only crate all others may import. This prevents cycles and keeps the foundation stable.

---

## 4. Crate Inventory & Justification

Every dependency is a maintenance burden, a potential supply-chain risk, and a compile-time cost. Each one below is here because it provides meaningful value that would be expensive to replicate correctly.

### Async Runtime

**`tokio`** (MIT)
The de facto async runtime for Rust. Mature, excellent ecosystem, well-audited. We use it for:
- SSH session tasks (each session is a spawned Tokio task)
- File I/O in storage operations
- PTY I/O bridging

Alternative considered: `async-std`. Rejected: smaller ecosystem, fewer integrations, russh targets tokio.

### UI

**`iced`** (MIT) — see Section 2 for full justification.

We will track Iced releases carefully. Pin to specific versions, update deliberately. Iced's API has stabilised significantly since 0.10.

### SSH

**`russh`** (MIT)
Pure-Rust SSH-2 protocol implementation. Active maintenance. Includes:
- All auth methods (password, publickey, keyboard-interactive, agent)
- SFTP subsystem
- Channel multiplexing
- Host key verification hooks

Alternative considered: `ssh2-rs` (bindings to libssh2). Rejected for three reasons:
1. FFI boundary is a security concern — memory safety guarantees weaken at the C boundary.
2. Dynamic linking to libssh2 complicates distribution and cross-compilation.
3. We cannot audit or contribute fixes to C code as easily as Rust code.

`russh` is used in production by Warp and others. It is the right choice.

### Terminal Emulation

**`vte`** (Apache-2.0)
A VT parser — it parses byte streams into VT/ANSI escape sequences and calls your handler. Used internally by Alacritty and others. We implement the `vte::Perform` trait to update our own terminal grid.

Why not use a higher-level library like `alacritty_terminal`? That crate is not published to crates.io for external consumption and is tightly coupled to Alacritty's rendering model. Building on `vte` directly means we own the grid implementation and can tailor it to our rendering approach without fighting another project's assumptions.

**`portable-pty`** (MIT) — from the WezTerm project
Cross-platform PTY creation (Linux: openpty, macOS: openpty, Windows: ConPTY). Used for the local shell (non-SSH) and potentially as an abstraction layer for the SSH pseudo-terminal. Published on crates.io. MIT licensed.

### Crypto & Memory Safety

**`zeroize`** (Apache-2.0 / MIT)
Guarantees that memory containing secrets is zeroed before deallocation, resisting compiler optimisation that would otherwise elide plain `memset` calls. This is non-negotiable for any application handling private keys or passwords.

**`secrecy`** (Apache-2.0 / MIT)
A thin wrapper (`Secret<T>`) that:
- Prevents `Debug` output from printing the contained value (prints `[REDACTED]`)
- Calls `zeroize` on drop
- Makes secret values explicit in the type system — you cannot accidentally pass a password where a username is expected

Use `secrecy::SecretString` for passwords in memory. Use `secrecy::SecretVec<u8>` for key material.

**`rand`** (MIT / Apache-2.0)
Cryptographically secure random number generation sourced from OS entropy. Used for session IDs, nonces, and similar. Do not use `rand::thread_rng()` for security-sensitive randomness — use `rand::rngs::OsRng` directly.

### Key Management

**`ssh-key`** (Apache-2.0 / MIT) — from the RustCrypto project
Parsing, serialisation, and generation of SSH key formats (OpenSSH, PEM). Supports Ed25519, RSA, ECDSA. Pure Rust. From the well-regarded RustCrypto organisation.

### Credential Storage

**`keyring`** (MIT / Apache-2.0)
Cross-platform OS keychain access:
- macOS: Keychain
- Windows: Windows Credential Manager
- Linux: Secret Service API (via D-Bus), with fallback to a file-based store

Passwords and key passphrases that the user chooses to save go here, never into config files.

### Configuration & Serialisation

**`serde`** + **`serde_derive`** (MIT / Apache-2.0)
Universal. All configuration types derive `Serialize`/`Deserialize`.

**`toml`** (MIT / Apache-2.0)
Human-readable config format. User-editable. Compatible with `~/.ssh/config` in spirit. Host profiles, app settings.

### Error Handling

**`thiserror`** (MIT / Apache-2.0)
For library-style errors in each crate. Derive macro for clean `Error` implementations.

**`anyhow`** (MIT / Apache-2.0)
For application-level error propagation with context. Used in `termite-app` and the binary, not in library crates.

### Logging & Tracing

**`tracing`** + **`tracing-subscriber`** (MIT)
Structured, async-aware logging. Sessions, connections, and operations are tagged with spans so logs are readable even with concurrent SSH sessions. **Never log secrets.** The `secrecy` crate enforces this at the type system level.

### SSH Config Parsing

**`ssh2-config`** or a custom parser (evaluate at implementation time)
OpenSSH's `~/.ssh/config` format needs to be parsed. Either find a maintained crate or implement it — the format is not complex and implementing it gives us full control over which directives we support.

### Testing

**`mockall`** (MIT / Apache-2.0) — for mocking traits in unit tests

**`proptest`** (MIT / Apache-2.0) — property-based testing, particularly useful for terminal emulator edge cases

**`criterion`** (MIT / Apache-2.0) — benchmarking for the terminal renderer and SSH throughput

### Summary table

| Crate | Purpose | License | Alternative considered |
|-------|---------|---------|----------------------|
| tokio | Async runtime | MIT | async-std (rejected) |
| iced | UI framework | MIT | egui, Tauri, Slint (all rejected) |
| russh | SSH protocol | MIT | ssh2-rs (rejected: FFI) |
| vte | VT parsing | Apache-2.0 | alacritty_terminal (rejected: not published) |
| portable-pty | PTY management | MIT | Manual per-OS (rejected: toil) |
| zeroize | Secret memory zeroing | Apache-2.0/MIT | Nothing comparable |
| secrecy | Secret type wrappers | Apache-2.0/MIT | Nothing comparable |
| rand / OsRng | CSPRNG | MIT/Apache-2.0 | Nothing comparable |
| ssh-key | SSH key format handling | Apache-2.0/MIT | Manual (rejected: format complexity) |
| keyring | OS keychain | MIT/Apache-2.0 | Manual per-OS (rejected: toil) |
| serde | Serialisation | MIT/Apache-2.0 | Nothing comparable |
| toml | Config format | MIT/Apache-2.0 | JSON (rejected: not human-friendly) |
| thiserror | Error types | MIT/Apache-2.0 | Manual (rejected: boilerplate) |
| anyhow | App error handling | MIT/Apache-2.0 | thiserror everywhere (wrong layer) |
| tracing | Structured logging | MIT | log (rejected: less async-aware) |
| mockall | Test mocking | MIT/Apache-2.0 | — |
| proptest | Property testing | MIT/Apache-2.0 | quickcheck (both fine) |
| criterion | Benchmarks | MIT/Apache-2.0 | — |

---

## 5. Module Responsibilities

### `termite-core`

The foundation. No dependencies on other workspace crates.

Contains:
- `SessionId` — a newtype wrapping `uuid::Uuid`
- `HostProfile` — name, host, port, username, auth method, tags
- `ConnectionStatus` — enum (Connecting, Connected, Reconnecting, Disconnected, Failed)
- `AuthMethod` — enum (Password, PublicKey { key_path }, Agent)
- `TermiteError` — top-level error enum used across crates
- Core traits: `KeyProvider`, `CredentialStore`
- Re-exports of `secrecy` and `zeroize` so all crates use the same versions

Nothing in `termite-core` does I/O, spawns tasks, or touches the filesystem.

### `termite-crypto`

Manages cryptographic key material.

Responsibilities:
- Load SSH private keys from disk (OpenSSH format, PEM)
- Decrypt passphrase-protected keys (passphrase from keychain or UI prompt)
- Generate new key pairs (Ed25519 preferred)
- Fingerprint computation for display in the UI
- Securely wipe key material from memory on drop
- Never log key bytes

Does NOT manage storage paths or keychain access — that is `termite-storage`.

### `termite-storage`

Manages all persistent state on disk.

Responsibilities:
- Host profiles: read/write TOML in `~/.config/termite/hosts/`
- App settings: `~/.config/termite/settings.toml`
- Known hosts: `~/.config/termite/known_hosts`
- Recent connections list
- Favourites
- Credential store implementation (wraps `keyring`)
- Migration between config schema versions

Config directory follows XDG on Linux, `~/Library/Application Support/termite` on macOS, `%APPDATA%\termite` on Windows.

### `termite-ssh`

The SSH protocol layer.

Responsibilities:
- Establish SSH connections via `russh`
- Authentication (password, publickey, agent forwarding)
- Host key verification against known hosts
- Channel management (shell, exec, subsystems)
- PTY negotiation for interactive sessions
- SFTP subsystem
- Port forwarding (local, remote, dynamic)
- Keep-alives
- Reconnection logic
- ProxyJump / ProxyCommand support
- SSH config file parsing (`~/.ssh/config`)
- Expose a clean async API: `SshSession::connect()`, `SshSession::open_shell()`, etc.

Each SSH session runs as a Tokio task. The session communicates with the app layer via `tokio::sync::mpsc` channels: `SessionEvent` (outbound, from session to app) and `SessionCommand` (inbound, from app to session).

Does NOT know about the UI, Iced, or the terminal grid.

### `termite-terminal`

The terminal emulator.

Responsibilities:
- VT/ANSI sequence parsing via `vte`
- Terminal grid: a 2D array of `Cell { character, fg, bg, attributes }`
- Scrollback buffer
- Selection (character, word, line, block)
- Clipboard integration (copy/paste)
- Mouse reporting
- Window title tracking
- Bell events
- Hyperlink detection
- Resize handling
- Local PTY management for non-SSH usage (via `portable-pty`)

The terminal grid is pure in-memory state. It takes bytes in and produces a renderable grid. It does not do rendering — the UI layer does.

### `termite-ui`

Reusable Iced components and the application theme.

Responsibilities:
- Color palette (dark mode first; light mode a future consideration)
- Typography configuration
- Custom widgets: TerminalView, TabBar, SidebarPanel, HostCard, CommandPalette, SplitView
- Animation helpers
- Keyboard shortcut registry
- Theme: accent colors, border radii, spacing scale

Has no knowledge of SSH sessions or storage. Takes data from `termite-app` and renders it.

### `termite-app`

The top-level application. Wires everything together.

Responsibilities:
- Implements `iced::Application`
- Owns `AppState` (all sessions, host list, UI state)
- Defines the `AppMessage` enum
- Routes messages between SSH layer, terminal layer, storage layer, and UI
- Manages the lifecycle of SSH session tasks
- Handles window events (resize, focus, close)
- Startup and shutdown logic
- Command palette action dispatch

### `src/main.rs`

```rust
fn main() -> iced::Result {
    termite_app::run()
}
```

That is all. No logic lives here.

---

## 6. Inter-module Communication

### The overall pattern

```
┌─────────────────────────────────────────────────────┐
│  Iced Event Loop (main thread)                       │
│  AppState  ←──── update(AppMessage) ────→ commands  │
└──────────────────────┬──────────────────────────────┘
                       │ mpsc channels
          ┌────────────┴────────────┐
          │                         │
   ┌──────▼──────┐          ┌───────▼───────┐
   │ SSH Task    │          │ SSH Task      │  (one per session)
   │ (tokio)     │          │ (tokio)       │
   └──────┬──────┘          └───────┬───────┘
          │ PTY bytes               │ PTY bytes
   ┌──────▼──────┐          ┌───────▼───────┐
   │ Terminal    │          │ Terminal      │  (one per session)
   │ Grid        │          │ Grid         │
   └─────────────┘          └───────────────┘
```

### Message types

**`AppMessage`** (defined in `termite-app`) — all Iced update calls receive this:

```rust
pub enum AppMessage {
    // Session lifecycle
    ConnectRequested(HostProfile),
    SessionEvent { id: SessionId, event: SessionEvent },
    DisconnectRequested(SessionId),
    
    // Terminal I/O
    TerminalInput { id: SessionId, data: Vec<u8> },
    TerminalResized { id: SessionId, rows: u16, cols: u16 },
    
    // UI
    TabSelected(SessionId),
    SidebarToggled,
    CommandPaletteOpened,
    CommandPaletteInput(String),
    CommandPaletteExecuted(Command),
    
    // Storage
    HostSaved(HostProfile),
    HostDeleted(HostId),
    
    // System
    WindowResized(u32, u32),
    CloseRequested,
}
```

**`SessionEvent`** (outbound from SSH task to app):

```rust
pub enum SessionEvent {
    Connected,
    AuthRequired(AuthChallenge),
    Output(Vec<u8>),           // raw bytes to feed to terminal grid
    Disconnected { reason: DisconnectReason },
    Error(SshError),
    HostKeyUnknown(HostKey),   // user must approve
    HostKeyMismatch(HostKey),  // security warning
}
```

**`SessionCommand`** (inbound to SSH task from app):

```rust
pub enum SessionCommand {
    Write(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    AuthResponse(AuthResponse),
    Disconnect,
}
```

### Channel topology

Each SSH session owns a pair of channels:
- `session_tx: mpsc::Sender<SessionCommand>` — app holds this to send commands
- `event_tx: mpsc::Sender<(SessionId, SessionEvent)>` — session uses this to send events to app

The app holds a single `event_rx` receiver subscribed to all sessions via an Iced `Subscription`. This is implemented using Iced's `subscription::channel` mechanism, which bridges async channels into the Iced update loop.

---

## 7. Terminal Emulation Architecture

The terminal emulator has three layers:

### Layer 1: PTY / Byte Source

For local shells: `portable-pty` creates a platform PTY and spawns a shell. Bytes flow bidirectionally.

For SSH sessions: `russh` provides channel I/O. The SSH channel behaves as the byte source. We request a PTY from the remote server and negotiate terminal dimensions.

Both sources are abstracted behind a trait in `termite-terminal`:

```rust
pub trait ByteSource: Send + 'static {
    async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    async fn write(&mut self, data: &[u8]) -> io::Result<()>;
    async fn resize(&mut self, rows: u16, cols: u16) -> io::Result<()>;
}
```

### Layer 2: VT Parser (`vte`)

Raw bytes from the byte source are fed into `vte::Parser::advance()`. This calls back into our `TerminalHandler` which implements `vte::Perform`. The handler updates the grid state.

### Layer 3: Terminal Grid

```rust
pub struct TerminalGrid {
    cells: Vec<Cell>,           // rows * cols cells, row-major
    rows: u16,
    cols: u16,
    cursor: CursorState,
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_limit: usize,
    scroll_offset: usize,       // 0 = bottom (live view)
    title: String,
    bell: bool,
    // colour state, SGR state, etc.
}

pub struct Cell {
    character: char,
    fg: Color,
    bg: Color,
    attrs: CellAttributes,      // bold, italic, underline, blink, reverse, etc.
    hyperlink: Option<Arc<str>>,
}
```

The terminal grid is queried by the UI layer to render the visible area. The UI layer reads the grid immutably on each Iced draw call.

### Rendering in Iced

The `TerminalView` widget in `termite-ui` is a custom Iced widget that:
1. Calculates visible cell range from widget dimensions and font metrics
2. Iterates cells and batches them into runs of the same style
3. Uses Iced's text primitives for rendering
4. Handles mouse events (click to position, drag to select, scroll)

For performance, the grid stores a dirty flag. On redraw, we only re-render if the grid is dirty since the last frame.

---

## 8. SSH Layer Architecture

### Session lifecycle

```
ConnectRequested(profile)
    │
    ▼
SshSession::spawn(profile, event_tx)
    │
    ├── DNS resolution
    ├── TCP connect
    ├── SSH handshake (version exchange, kex)
    ├── Host key verification ──► HostKeyUnknown / HostKeyMismatch event
    ├── Authentication ──────────► AuthRequired event if needed
    ├── Open session channel
    ├── Request PTY
    ├── Start shell
    │
    └── Loop: read stdout → Output event
              read stderr → Output event (merged or separate TBD)
              write stdin ← SessionCommand::Write
              resize ←────── SessionCommand::Resize
```

### Authentication flow

Authentication is handled via a state machine. The SSH handshake can require multiple rounds (e.g., keyboard-interactive challenges). The session task fires `SessionEvent::AuthRequired(AuthChallenge)` and waits for `SessionCommand::AuthResponse` from the app layer. The app layer shows the relevant UI (password prompt, TOTP prompt, etc.) and responds.

Private keys are loaded by `termite-crypto` before the session starts. The session receives a `Box<dyn KeyProvider>` rather than raw key material — it calls `provider.sign(data)` and gets the signature back, never touching the raw private key bytes directly.

### ProxyJump

ProxyJump creates a nested SSH session. The outer session opens a TCP channel to the target host, and the inner session runs over that channel. The implementation creates two `SshSession` objects; the inner session's byte source is the outer session's channel rather than a TCP socket. This is the correct model and avoids subprocess spawning.

### Host key verification

Every connection verifies the server host key against `~/.config/termite/known_hosts`. If the key is unknown, we fire `SessionEvent::HostKeyUnknown` and the UI shows a first-connection dialog with the key fingerprint. The user must explicitly approve.

If the key has changed, we fire `SessionEvent::HostKeyMismatch` and show a prominent security warning. We do NOT connect silently. `StrictHostKeyChecking` is effectively always `ask` — we never silently accept changed keys.

---

## 9. Security Architecture

### Threat model

We assume:
- The user's machine is not fully compromised (if it is, game over for any credential manager)
- The user's config directory may be readable by other processes on the machine
- The network between client and server may be adversarial
- The SSH server may present a fraudulent identity on first connection (TOFU risk)

We do not assume:
- The SSH server is trustworthy on first connection
- The user's clipboard is private

### Memory

All secret types (`SecretString`, `SecretVec<u8>`) are wrapped in `secrecy::Secret<T>`, which:
- Zeroes memory on drop (via `zeroize`)
- Redacts value in `Debug` output
- Requires explicit `.expose_secret()` call to access the inner value — every access is visible in code review

Never store secrets in `String`, `Vec<u8>`, or any type that does not implement `Zeroize`.

Never log secrets. The `tracing` calls must never include secret values. Since `secrecy` redacts `Debug`, including a `SecretString` in a log line is safe, but avoid it anyway.

### Key material

Private keys are loaded from disk into `secrecy::SecretVec<u8>`. The decrypted key material is passed to the signing function and the `SecretVec` is dropped immediately after — not held in the session state longer than needed.

### Configuration files

Config files (`hosts/*.toml`, `settings.toml`) never contain secrets. Passwords and key passphrases are stored in the OS keychain. The config may contain usernames, hostnames, and key file paths. These are not considered secret.

Known hosts (`known_hosts`) is not secret but should not be casually writable. On Linux/macOS, restrict file permissions to 0600.

### Dependencies

Run `cargo audit` in CI on every push. Reject dependencies with known CVEs. Pin `Cargo.lock`. The `audit.yml` workflow runs on a schedule to catch newly published advisories.

Run `cargo deny` to enforce:
- License allow-list (MIT, Apache-2.0, BSD-2/3, ISC)
- Dependency duplication limits
- Advisory bans

### Timing attacks

Credential comparison (e.g., comparing a stored fingerprint to an observed fingerprint) must use constant-time comparison. Use `subtle::ConstantTimeEq` for any security-relevant comparison. This is automatically handled within `russh` for most SSH operations, but be vigilant in our own code.

---

## 10. Credential & Key Storage

### Decision matrix

| Data type | Stored where | Format |
|-----------|-------------|--------|
| Host profiles | `~/.config/termite/hosts/*.toml` | TOML, no secrets |
| App settings | `~/.config/termite/settings.toml` | TOML |
| Known hosts | `~/.config/termite/known_hosts` | OpenSSH-compatible format |
| SSH private keys | `~/.config/termite/keys/` | OpenSSH format, encrypted |
| Key passphrases (if saved) | OS keychain | `keyring` service `termite`, key = fingerprint |
| Host passwords (if saved) | OS keychain | `keyring` service `termite`, key = `host:user` |

### The `CredentialStore` trait

```rust
pub trait CredentialStore: Send + Sync {
    async fn get_password(&self, host: &str, user: &str) 
        -> Result<Option<SecretString>>;
    async fn set_password(&self, host: &str, user: &str, password: SecretString) 
        -> Result<()>;
    async fn delete_password(&self, host: &str, user: &str) 
        -> Result<()>;
    async fn get_passphrase(&self, fingerprint: &str) 
        -> Result<Option<SecretString>>;
    async fn set_passphrase(&self, fingerprint: &str, passphrase: SecretString) 
        -> Result<()>;
}
```

A `KeyringStore` implements this trait using the `keyring` crate. A `MemoryStore` implements it for testing. The SSH layer receives a `Box<dyn CredentialStore>` — it never calls `keyring` directly.

### SSH agent integration

If an SSH agent is running (`$SSH_AUTH_SOCK` on Unix), we connect to it and present it as an alternative `KeyProvider`. This is the lowest-friction auth path for users who already have an agent. No key passphrase prompts needed.

---

## 11. State Management

The top-level application state (in `termite-app`):

```rust
pub struct AppState {
    // Active sessions
    sessions: IndexMap<SessionId, ActiveSession>,
    tab_order: Vec<SessionId>,
    active_tab: Option<SessionId>,
    
    // Host management
    host_store: Arc<dyn HostStore>,
    recent: VecDeque<HostProfile>,
    favourites: Vec<HostId>,
    
    // UI state
    sidebar_open: bool,
    command_palette: CommandPaletteState,
    modal: Option<Modal>,         // e.g., host key approval, password prompt
    
    // Persistent settings
    settings: Settings,
    
    // Async channel — receives events from all session tasks
    event_rx: mpsc::Receiver<(SessionId, SessionEvent)>,
}

pub struct ActiveSession {
    profile: HostProfile,
    status: ConnectionStatus,
    terminal: TerminalGrid,
    command_tx: mpsc::Sender<SessionCommand>,
    split: Option<Box<ActiveSession>>,   // for split terminals
}
```

State changes happen only through the Iced `update(message)` function. There is no shared mutable state accessed concurrently in the Iced layer. The SSH tasks are isolated in Tokio and communicate only through channels.

---

## 12. Performance Considerations

### Terminal rendering

- The terminal grid is allocated once at connection time and cells are updated in-place
- Avoid per-frame allocations in the render path
- Dirty tracking: only re-render when the grid changed since the last frame
- Batch text rendering into style runs (sequences of cells with the same fg/bg/attrs)
- Scrollback buffer is capped (default: 10,000 lines, configurable)

### SSH throughput

- `russh` is async and non-blocking; PTY bytes are streamed to the terminal grid via a channel
- Terminal grid updates do not happen on the Iced main thread; they happen in the session task, and the UI polls the grid for render
- For high-throughput sessions (e.g., `cat bigfile`), batch output into chunks before updating the grid rather than processing one byte at a time

### Startup time

- Host profiles are loaded lazily (on demand, not all at startup)
- The window should appear within 200ms on all platforms
- Do not block the main thread during startup; load settings async

### Memory

- Sessions are isolated. A session's terminal grid does not share memory with other sessions.
- The scrollback buffer is the largest consumer. 10,000 lines × 220 columns × ~40 bytes/cell ≈ 88MB per session at default settings. Consider a configurable limit and a compressed scrollback option in a future milestone.

---

## 13. Testing Strategy

### Unit tests

Every module has `#[cfg(test)] mod tests` blocks. Focus on:
- `termite-terminal`: VT parser output for specific escape sequences. Use test vectors from the vttest suite.
- `termite-crypto`: Key loading from test fixtures, fingerprint computation.
- `termite-storage`: Config serialisation round-trip.
- `termite-ssh`: Auth state machine transitions.
- `termite-app`: Message routing, state transitions for known sequences.

### Integration tests

- `tests/` directory in `termite-ssh`: Start an in-process SSH server (or mock server), connect, auth, and verify shell output. `russh` includes utilities for writing SSH server stubs.
- `tests/` for terminal: Feed known input streams and compare rendered grid output.

### Property tests (`proptest`)

- Terminal grid: arbitrary byte sequences should not panic (soundness under malformed input)
- Config: serialise→deserialise round-trips for all config types
- SSH message parsing: arbitrary bytes into protocol parsers should not panic

### Fuzz targets (`cargo-fuzz`)

Priority fuzz targets:
1. The VT parser (`vte::Parser::advance` with arbitrary input)
2. SSH host key parsing
3. SFTP response parsing

Fuzzing should be set up from the start even if not run continuously. Add to CI later.

### Security tests

- Verify that `Debug` output of `SecretString` and `SecretVec` does not contain secret values
- Verify that `zeroize` is called on drop by checking memory patterns in tests (challenging; at minimum assert the type implements `ZeroizeOnDrop`)
- Verify known-hosts mismatch fires the right event rather than silently connecting

### Benchmarks (`criterion`)

- Terminal grid throughput: bytes/sec for a representative VT-heavy workload
- SSH channel throughput: latency and throughput for data channels
- UI render time: time to produce a frame from a fully populated terminal grid

---

## 14. Licensing

Termite is released under the **MIT License**.

Reasoning:
- Maximally permissive for downstream use and embedding
- Compatible with all our dependencies (all MIT or Apache-2.0)
- Encourages adoption and contribution
- No copyleft obligations for users who build internal forks

No GPL or LGPL dependencies. The `cargo deny` configuration enforces this automatically.

The SPDX identifier for the project is `MIT`.

All source files should have an SPDX header:
```rust
// SPDX-License-Identifier: MIT
```

---

## 15. Development Roadmap

Each milestone has a clear definition of done. A milestone is not complete until all items are ticked.

### M0 — Project Skeleton

*Goal: a compiling, CI-green skeleton with a window that opens.*

- [ ] Cargo workspace with all 7 crates
- [ ] CI: `cargo clippy --all`, `cargo test --all`, build matrix (Linux/macOS/Windows) on GitHub Actions
- [ ] `cargo deny` configuration (license allow-list, advisory bans)
- [ ] `cargo audit` in CI
- [ ] Empty crate APIs stubbed with `todo!()` implementations
- [ ] Basic Iced window opens on all 3 platforms with a placeholder view
- [ ] `ARCHITECTURE.md`, `CONTRIBUTING.md`, `LICENSE`, `README.md` committed
- [ ] Git commit template and `.gitignore`

*Estimated: 1–2 weeks*

---

### M1 — Local Terminal

*Goal: a fully functional terminal emulator attached to a local shell. No SSH yet.*

- [ ] `TerminalGrid` implementation with VT parsing via `vte`
- [ ] `portable-pty` integration for local shell
- [ ] Custom Iced `TerminalView` widget renders the grid
- [ ] Keyboard input routed to PTY
- [ ] ANSI 16-colour support
- [ ] 256-colour support
- [ ] True colour (24-bit) support
- [ ] Bold, italic, underline, reverse video, dim
- [ ] Cursor rendering
- [ ] UTF-8 support including multi-column characters (CJK)
- [ ] Emoji rendering
- [ ] Terminal resize on window resize
- [ ] Scrollback with scroll wheel
- [ ] Basic clipboard: copy selection, paste

*This milestone is the hardest technically. Budget extra time.*

*Estimated: 3–4 weeks*

---

### M2 — SSH Core

*Goal: connect to a remote host over SSH with password auth and see a live terminal.*

- [ ] `russh`-based SSH client in `termite-ssh`
- [ ] TCP + SSH handshake
- [ ] Password authentication
- [ ] Host key verification against known hosts file
- [ ] First-connection approval dialog in UI
- [ ] Changed host key warning (prominent)
- [ ] SSH channel → terminal grid integration
- [ ] Terminal resize propagated to remote PTY (SIGWINCH equivalent)
- [ ] Quick connect dialog (host, port, user)
- [ ] Disconnect handling

*Estimated: 2–3 weeks*

---

### M3 — Key Authentication & Credential Storage

*Goal: auth with SSH keys, store credentials securely.*

- [ ] Ed25519 key loading
- [ ] RSA key loading
- [ ] ECDSA key loading
- [ ] Passphrase-protected key decryption
- [ ] Passphrase prompt in UI
- [ ] Optional passphrase storage in OS keychain
- [ ] SSH agent forwarding (`$SSH_AUTH_SOCK` / Pageant)
- [ ] Optional password save in OS keychain
- [ ] `~/.ssh/config` parsing (Host, HostName, User, Port, IdentityFile, ProxyJump)
- [ ] Key generation UI (Ed25519, optional comment, optional passphrase)

*Estimated: 2–3 weeks*

---

### M4 — Host Management

*Goal: organised host list, favourites, recent connections.*

- [ ] Host profile CRUD (create, read, update, delete)
- [ ] Sidebar with host list
- [ ] Favourites (star a host)
- [ ] Recent connections (persist across sessions)
- [ ] Search/filter hosts
- [ ] Host tags
- [ ] Host import from `~/.ssh/config`
- [ ] Host profile editor UI

*Estimated: 2–3 weeks*

---

### M5 — Tabs & Multi-Session

*Goal: multiple concurrent SSH sessions in tabs.*

- [ ] Tab bar
- [ ] Open new session tab
- [ ] Close tab
- [ ] Reorder tabs
- [ ] Keyboard shortcuts for tab navigation (Ctrl+Tab, Ctrl+1–9)
- [ ] Each session fully isolated (disconnect one does not affect others)
- [ ] Session-level reconnect (manual and automatic with back-off)
- [ ] Session status indicator in tab (connected / reconnecting / disconnected)

*Estimated: 2 weeks*

---

### M6 — Advanced Terminal Features

*Goal: feature-complete terminal emulation.*

- [ ] Mouse reporting (click, drag, wheel — all modes: X10, button, any-event)
- [ ] Hyperlink support (OSC 8)
- [ ] Bracketed paste mode
- [ ] Alternate screen buffer (full-screen apps: vim, htop, etc.)
- [ ] `xterm`-style title setting (OSC 0/2)
- [ ] Bell (visual flash, optional audio)
- [ ] Selection modes: character, word, line
- [ ] Search in scrollback (regex)
- [ ] Split terminal (horizontal and vertical)

*Estimated: 3 weeks*

---

### M7 — Advanced SSH Features

*Goal: port forwarding, SFTP, and jump hosts.*

- [ ] Local port forwarding (`-L`)
- [ ] Remote port forwarding (`-R`)
- [ ] Port forwarding management UI (list, add, remove active forwardings)
- [ ] ProxyJump support
- [ ] ProxyCommand support (spawn subprocess)
- [ ] SFTP file browser panel
- [ ] Upload file via SFTP
- [ ] Download file via SFTP
- [ ] Keep-alive configuration (server-alive interval)
- [ ] Connection compression (`Compression yes`)

*Estimated: 4 weeks*

---

### M8 — UX Polish & Command Palette

*Goal: the application feels premium and keyboard-native.*

- [ ] Command palette (Ctrl+P): search and run any action
- [ ] Full keyboard shortcut system (configurable)
- [ ] Session-level themes (per-host accent colour)
- [ ] Smooth tab transitions
- [ ] Connection animation (subtle)
- [ ] Font selection (monospace fonts on the system)
- [ ] Font size adjustment (Ctrl+= / Ctrl+-)
- [ ] Zoom to session (maximise one terminal)
- [ ] Onboarding flow (first run)
- [ ] Keyboard shortcut reference (Ctrl+?)

*Estimated: 3 weeks*

---

### M9 — Quality, Security, & Public Release

*Goal: production-quality. Ready for public announcement.*

- [ ] `cargo-fuzz` targets for VT parser and SSH message parsing
- [ ] Full proptest coverage for terminal and config
- [ ] `cargo-flamegraph` profiling pass; resolve top 3 hot paths
- [ ] Scrollback memory optimisation (configurable limit, consider compression)
- [ ] Security review pass: audit all secret handling paths
- [ ] `SECURITY.md` with responsible disclosure policy
- [ ] Distribution packaging:
  - Linux: AppImage + `.deb` + `.rpm`
  - macOS: `.dmg` with notarisation
  - Windows: NSIS installer + portable `.exe`
- [ ] Signed releases
- [ ] `CHANGELOG.md`
- [ ] Contributing guide reviewed for external contributors
- [ ] GitHub Discussions or Matrix room for community

*Estimated: 4–6 weeks (can be parallelised with M8)*

---

### Future milestones (not scheduled)

These are desirable but out of scope for the initial public release:

- **Light mode theme**
- **Plugin/extension API** (load additional protocol handlers or UI panels)
- **Mosh support** (UDP-based, different protocol)
- **tmux integration** (named sessions, detach/reattach)
- **Snippet / macro system** (send text snippets with keyboard shortcuts)
- **Audit log** (optional: log commands sent per session)
- **Multi-hop SFTP** (browse files through ProxyJump chains)
- **Terminal recording/playback** (asciinema-compatible)

---

*This document was last updated at project inception. It should be updated whenever a decision changes.*
