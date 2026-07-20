// SPDX-License-Identifier: MIT
//! Top-level application state and Iced wiring for Termite.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iced::futures::channel::mpsc as bridge;
use iced::futures::{SinkExt, Stream, StreamExt};
use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use iced::widget::text::{Rich, Span};
use iced::widget::{column, container, mouse_area, row, Stack};
use iced::{stream, Element, Font, Length, Subscription, Task, Theme};
use secrecy::SecretString;

use termite_core::{
    AuthMethod, ConnectionStatus, CredentialStore, HostProfile, SessionId, TabId, TermiteError,
};
use termite_ssh::{
    AuthChallenge, AuthResponse, DisconnectReason, HostConfig, HostKey, SessionCommand,
    SessionEvent, SshConfig, SshSession,
};
use termite_storage::{HostStore, KeyringStore, MemoryHostStore, TomlHostStore};
use termite_terminal::{GridHandler, MouseTracking, Pty, TerminalGrid};
use termite_ui::{
    prompt, sidebar, tabbar, AuthKind, Prompt, PromptMessage, SidebarMessage, SidebarState,
    TabBarMessage, TabSummary,
};

/// Grid size before the first `Message::WindowResized` arrives (Iced has no
/// synchronous "give me the current window size" call at startup — see
/// `grid_size_for_window`, which takes over from here once the window
/// reports its real size).
const ROWS: usize = 30;
const COLS: usize = 100;

/// How often each local tab's output buffer is drained and fed to its VT
/// parser.
const POLL_INTERVAL: Duration = Duration::from_millis(16);

/// How many consecutive automatic reconnect attempts a dropped SSH tab gets
/// before it's left `Disconnected` for the user to retry manually.
const MAX_RECONNECT_ATTEMPTS: u32 = 6;

/// Approximate cell metrics for `Font::MONOSPACE` at the terminal's text
/// size (14, see `view`), used to translate a window resize in pixels into
/// a row/column count. Not measured against the actual font — Iced has no
/// synchronous glyph-metrics query available at this layer — so this is a
/// deliberate approximation, not pixel-perfect; see `grid_size_for_window`.
const CELL_WIDTH_PX: f32 = 8.4;
const CELL_HEIGHT_PX: f32 = 18.0;

/// Fixed chrome the terminal pane doesn't get to draw into: the sidebar's
/// width (`sidebar::view`'s `Length::Fixed(220.0)`) and an estimate of the
/// tab bar's height.
const SIDEBAR_WIDTH_PX: f32 = 220.0;
const CHROME_HEIGHT_PX: f32 = 40.0;

/// How long a bell's visual flash stays on before `Message::BellTimeout`
/// clears it.
const BELL_FLASH_DURATION: Duration = Duration::from_millis(200);

/// xterm mouse-report button codes not covered by a real pressed button:
/// the "no button" placeholder used for plain motion reports (`AnyEvent`
/// tracking with nothing held), and the two wheel codes. Left/Middle/Right
/// press codes are computed from `iced::mouse::Button` instead (see
/// `mouse_button_code`) since they're already contiguous small integers.
const MOUSE_NO_BUTTON: u8 = 3;
const MOUSE_WHEEL_UP: u8 = 64;
const MOUSE_WHEEL_DOWN: u8 = 65;
/// Added to a button code to mark a motion report rather than a press.
const MOUSE_MOTION_FLAG: u8 = 32;

/// How close together (in time) two left-clicks on the same cell need to be
/// to chain into a double/triple click, cycling `SelectionMode` through
/// character → word → line (see `click_selection_mode`).
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);

// ── Tabs ──────────────────────────────────────────────────────────────────────

/// What a tab's byte source is. A local tab owns its own PTY end-to-end; an
/// SSH tab's bytes flow through the `ssh_worker` subscription instead, keyed
/// by `session_id`.
enum TabKind {
    Local {
        writer: Box<dyn Write + Send>,
        output: Arc<Mutex<Vec<u8>>>,
        /// Kept alive so the pty and child shell aren't torn down; also the
        /// target of a window resize (see `resize_all_tabs`).
        pty: Pty,
    },
    Ssh {
        /// `None` between the tab being created and the `SessionSpawned`
        /// message confirming the id `SshSession::spawn` assigned it (the
        /// worker generates the id itself; see `ssh_worker`). Also cleared
        /// back to `None` while a reconnect is pending, since the old
        /// session is dead and nothing should be sent to it.
        session_id: Option<SessionId>,
        profile: HostProfile,
        reconnect_attempt: u32,
    },
}

impl TabKind {
    fn session_id(&self) -> Option<SessionId> {
        match self {
            TabKind::Local { .. } => None,
            TabKind::Ssh { session_id, .. } => *session_id,
        }
    }
}

/// How much of a mouse-drag gesture on the terminal pane counts as
/// "selected", cycled through by clicking the same cell repeatedly within
/// `DOUBLE_CLICK_WINDOW` (see `click_selection_mode`) — the usual
/// click/double-click/triple-click convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionMode {
    Character,
    Word,
    Line,
}

/// A local text-selection gesture on a tab's terminal pane: purely a
/// UI-input concern (like `bell_flash`), not something `TerminalGrid` needs
/// to know about — it never leaves this process, unlike xterm mouse
/// reporting's bytes. `anchor` is where the gesture started, `head` is where
/// the mouse currently is (or was on release); either may come before the
/// other in reading order, so rendering/copy always normalize via
/// `selection_range`. Both are 0-indexed `(row, col)` grid coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    anchor: (usize, usize),
    head: (usize, usize),
    mode: SelectionMode,
}

/// One open tab: its own terminal grid/parser plus whatever's feeding it.
struct Tab {
    id: TabId,
    title: String,
    grid: TerminalGrid,
    parser: vte::Parser,
    kind: TabKind,
    status: ConnectionStatus,
    /// Whether this tab's bell flash is currently showing (see
    /// `Message::BellTimeout`). Purely a UI-timing concern, so it lives on
    /// `Tab` rather than `TerminalGrid`, which only records that a bell
    /// happened (`TerminalGrid::take_bell`).
    bell_flash: bool,
    /// This tab's in-progress or most recently finished local text
    /// selection, if any (see `Selection`). Cleared on a plain click with no
    /// drag; anything else survives until the next click starts a new one.
    selection: Option<Selection>,
}

impl Tab {
    fn new(
        title: impl Into<String>,
        kind: TabKind,
        status: ConnectionStatus,
        rows: usize,
        cols: usize,
    ) -> Self {
        Self {
            id: TabId::new(),
            title: title.into(),
            grid: TerminalGrid::new(rows, cols),
            parser: vte::Parser::new(),
            kind,
            status,
            bell_flash: false,
            selection: None,
        }
    }

    /// Feeds raw bytes through this tab's own VT parser into its own grid.
    /// Returns whether a bell rang while processing them.
    fn advance(&mut self, bytes: &[u8]) -> bool {
        let mut handler = GridHandler {
            grid: &mut self.grid,
        };
        for &byte in bytes {
            self.parser.advance(&mut handler, byte);
        }
        self.grid.take_bell()
    }
}

// ── Application state ─────────────────────────────────────────────────────────

/// Root application state.
///
/// Extended in M1+ as sessions, host profiles, and UI state are added.
pub struct TermiteApp {
    // ── M5: tabs ────────────────────────────────────────────────────
    /// Every open tab, in display order. Never empty after `new()` — closing
    /// the last tab immediately reopens a fresh local one (see `close_tab`).
    tabs: Vec<Tab>,
    /// Which tab is receiving keystrokes and shown in the terminal pane.
    /// Only `None` in the brief window during `new()` before the first tab
    /// is pushed.
    active_tab: Option<TabId>,
    /// The row/column size every tab is currently created and kept sized
    /// to, kept up to date by `Message::WindowResized` (see
    /// `grid_size_for_window`). Starts at the `ROWS`/`COLS` fallback until
    /// the first real window-size event arrives.
    grid_size: (usize, usize),
    /// Last known cursor position within the terminal pane's `mouse_area`
    /// (local to that widget, so no chrome offset needed — see `cell_at`).
    /// `None` before the first `Message::MouseMoved`, or once the cursor
    /// has left the area. Presses/releases/scrolls read this rather than
    /// carrying their own position, since Iced's `MouseArea` doesn't hand
    /// one to `on_press`/`on_release`/`on_scroll` (see `view`).
    mouse_position: Option<iced::Point>,
    /// The button currently held down over the terminal pane, if any —
    /// needed to pick the right code for a motion report under
    /// `MouseTracking::ButtonEvent` (see `report_mouse_event`).
    mouse_button_down: Option<iced::mouse::Button>,
    /// When and where (in grid cells) the most recent left-click landed,
    /// used to detect a chained double/triple click for
    /// `SelectionMode::Word`/`Line` (see `click_selection_mode`). `None`
    /// once a click lands too late or on a different cell to chain.
    last_click: Option<(std::time::Instant, (usize, usize), u8)>,

    // ── M4: host management ──────────────────────────────────────────
    host_store: Arc<dyn HostStore>,
    hosts: Vec<HostProfile>,
    sidebar: SidebarState,
    /// Parsed `~/.ssh/config`, used to resolve `Host` aliases typed into the
    /// add-host form's address field (see `SidebarMessage::ResolveAlias`).
    /// Loaded once at startup; a file that changes after launch isn't
    /// picked up until the app restarts, same as saved host profiles.
    ssh_config: SshConfig,
    /// OS keychain access for the credential prompt's "Save to keychain"
    /// toggle (see `saved_credential`/`save_credential`).
    credential_store: Arc<dyn CredentialStore>,

    // ── M4: SSH session wiring ────────────────────────────────────────
    /// Sender into the persistent SSH worker subscription; `None` until its
    /// first poll delivers `Message::SshWorkerReady`.
    ssh_worker: Option<bridge::Sender<SshWorkerInput>>,
    /// A credential or host-key prompt currently blocking a session,
    /// waiting on the user. `None` means no modal is shown. Only one prompt
    /// is surfaced at a time; per `CLAUDE.md`'s no-silent-accept invariant,
    /// a second prompt arriving while one is already pending fails closed
    /// (see `handle_session_event`) rather than silently overwriting it.
    pending_prompt: Option<PendingPrompt>,
}

/// A prompt awaiting the user's decision, plus enough of the originating
/// `SessionEvent` to act on that decision once it's made. `ui` is the plain,
/// SSH-type-free display data `termite_ui::prompt::view` renders — embedded
/// directly (rather than derived on each `view()` call) so the modal's
/// `Element` can borrow it with `app`'s lifetime instead of a temporary's.
#[derive(Debug)]
enum PendingPrompt {
    Credential {
        session: SessionId,
        challenge: AuthChallenge,
        ui: Prompt,
    },
    HostKey {
        session: SessionId,
        ui: Prompt,
    },
}

impl PendingPrompt {
    fn ui(&self) -> &Prompt {
        match self {
            PendingPrompt::Credential { ui, .. } => ui,
            PendingPrompt::HostKey { ui, .. } => ui,
        }
    }
}

impl TermiteApp {
    fn new() -> Result<Self, termite_terminal::PtyError> {
        let tab = spawn_local_tab(ROWS, COLS)?;
        let active_tab = Some(tab.id);

        Ok(Self {
            tabs: vec![tab],
            active_tab,
            grid_size: (ROWS, COLS),
            mouse_position: None,
            mouse_button_down: None,
            last_click: None,
            host_store: make_host_store(),
            hosts: Vec::new(),
            sidebar: SidebarState::default(),
            ssh_config: load_ssh_config(),
            credential_store: Arc::new(KeyringStore::new()),
            ssh_worker: None,
            pending_prompt: None,
        })
    }

    fn find_tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    fn find_tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    fn find_tab_by_session_mut(&mut self, session_id: SessionId) -> Option<&mut Tab> {
        self.tabs
            .iter_mut()
            .find(|tab| tab.kind.session_id() == Some(session_id))
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.active_tab.and_then(|id| self.find_tab(id))
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.active_tab.and_then(move |id| self.find_tab_mut(id))
    }
}

/// Spawns a new local shell PTY, its background reader thread, and wraps
/// them into a fresh [`Tab`]. Used both for the app's initial tab and for
/// `Message::NewLocalTab`; sized to `rows`/`cols` (the app's current
/// `grid_size`, so a new tab matches whatever size the window has already
/// settled on).
fn spawn_local_tab(rows: usize, cols: usize) -> Result<Tab, termite_terminal::PtyError> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let pty = Pty::spawn(&shell, rows as u16, cols as u16)?;

    let mut reader = pty.try_clone_reader()?;
    let writer = pty.take_writer()?;

    let output = Arc::new(Mutex::new(Vec::new()));
    let reader_output = Arc::clone(&output);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut output) = reader_output.lock() {
                        output.extend_from_slice(&buf[..n]);
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(Tab::new(
        "Local",
        TabKind::Local {
            writer,
            output,
            pty,
        },
        ConnectionStatus::Connected,
        rows,
        cols,
    ))
}

/// Feeds `bytes` into the active tab's grid, for banner-style status text
/// (import results, key-generation results) that isn't tied to any
/// particular session. A no-op if there's no active tab (shouldn't happen
/// outside tests).
fn advance_active(app: &mut TermiteApp, bytes: &[u8]) {
    if let Some(tab) = app.active_tab_mut() {
        tab.advance(bytes);
    }
}

/// Handles `Message::NewLocalTab`: spawns a new local tab and makes it
/// active. A spawn failure is logged and otherwise ignored — the app
/// already has at least one tab open, so there's nothing to fail closed.
fn new_local_tab(app: &mut TermiteApp) {
    let (rows, cols) = app.grid_size;
    match spawn_local_tab(rows, cols) {
        Ok(tab) => {
            app.active_tab = Some(tab.id);
            app.tabs.push(tab);
        }
        Err(err) => tracing::error!(%err, "failed to spawn new local shell tab"),
    }
}

/// The real on-disk store where the platform has a config dir, falling back
/// to an in-memory store (e.g. headless/sandboxed test environments) rather
/// than failing startup over host profile persistence.
fn make_host_store() -> Arc<dyn HostStore> {
    match TomlHostStore::default_path() {
        Some(path) => Arc::new(TomlHostStore::new(path)),
        None => {
            tracing::warn!("no platform config directory; host profiles won't persist");
            Arc::new(MemoryHostStore::new())
        }
    }
}

/// Loads and parses `~/.ssh/config` for alias resolution in the add-host
/// form. Read synchronously — same reasoning as the PTY spawn a few lines
/// above `new()`'s call site: it's a small local file, not worth a `Task`.
/// No config file resolves to an empty `SshConfig` (see `SshConfig::load`);
/// a malformed one logs a warning and also resolves empty rather than
/// failing app startup over a form-autofill convenience feature.
fn load_ssh_config() -> SshConfig {
    match termite_ssh::ssh_config::default_path() {
        Some(path) => SshConfig::load(&path).unwrap_or_else(|err| {
            tracing::warn!(%err, "failed to parse ~/.ssh/config; alias resolution disabled");
            SshConfig::default()
        }),
        None => SshConfig::default(),
    }
}

/// Loads all saved host profiles off the main thread.
fn load_hosts_task(store: Arc<dyn HostStore>) -> Task<Message> {
    Task::perform(list_hosts(store), Message::HostsLoaded)
}

async fn list_hosts(store: Arc<dyn HostStore>) -> Vec<HostProfile> {
    tokio::task::spawn_blocking(move || {
        store.list().unwrap_or_else(|err| {
            tracing::error!(%err, "failed to list host profiles");
            Vec::new()
        })
    })
    .await
    .unwrap_or_default()
}

/// Orders the sidebar's host list: starred hosts first, then by most recent
/// connection (never-connected hosts sort last within their tier), ties
/// broken alphabetically. The sidebar itself renders whatever order it's
/// given (see its own doc comment) — this is the one place display order is
/// decided, applied every time `Message::HostsLoaded` delivers a fresh list.
fn sort_hosts(hosts: &mut [HostProfile]) {
    hosts.sort_by(|a, b| {
        b.favourite
            .cmp(&a.favourite)
            .then_with(|| b.last_connected.cmp(&a.last_connected))
            .then_with(|| {
                a.name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase())
            })
    });
}

/// Current time as a unix timestamp, for `HostProfile::last_connected`. `0`
/// on a clock that reports before the epoch — practically never — rather
/// than propagating an error over a recency-sort nicety.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Requests sent from the app to the persistent SSH worker subscription
/// (see [`ssh_worker`]).
#[derive(Debug, Clone)]
pub enum SshWorkerInput {
    /// Spawn a new session connecting to this host profile, for the tab
    /// identified by `TabId` (which already exists in `TermiteApp::tabs` by
    /// the time this is sent — see `update_sidebar`'s `SelectHost` arm and
    /// `reconnect`). The worker echoes the resulting `SessionId` back via
    /// `Message::SessionSpawned` since it — not the app — assigns it.
    Connect(TabId, HostProfile),
    /// Forward a command to an already-spawned session.
    Send(SessionId, SessionCommand),
}

/// Runs for the lifetime of the app as an Iced subscription. Owns every
/// spawned [`SshSession`]'s command sender, keyed by [`SessionId`], and
/// multiplexes their events back to the app — mirroring the channel
/// topology in `ARCHITECTURE.md` §6 (one shared `event_tx` per app, one
/// `command_tx` per session).
///
/// The app has no direct handle to this state; it talks to it only via the
/// `bridge::Sender<SshWorkerInput>` delivered in `Message::SshWorkerReady`
/// on the first poll, per iced's documented pattern for bidirectional
/// subscription workers.
fn ssh_worker() -> impl Stream<Item = Message> {
    stream::channel(100, |mut output| async move {
        let (input_tx, mut input_rx) = bridge::channel(32);
        if output
            .send(Message::SshWorkerReady(input_tx))
            .await
            .is_err()
        {
            return;
        }

        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(32);
        let mut sessions: HashMap<SessionId, tokio::sync::mpsc::Sender<SessionCommand>> =
            HashMap::new();

        loop {
            tokio::select! {
                input = input_rx.next() => {
                    match input {
                        Some(SshWorkerInput::Connect(tab_id, profile)) => {
                            match termite_ssh::known_hosts::known_hosts_path() {
                                Ok(known_hosts_path) => {
                                    let (id, command_tx) =
                                        SshSession::spawn(profile, known_hosts_path, event_tx.clone());
                                    sessions.insert(id, command_tx);
                                    if output.send(Message::SessionSpawned(tab_id, id)).await.is_err() {
                                        break;
                                    }
                                }
                                Err(err) => {
                                    tracing::error!(%err, "cannot resolve known_hosts path");
                                }
                            }
                        }
                        Some(SshWorkerInput::Send(id, command)) => {
                            if let Some(command_tx) = sessions.get(&id) {
                                let _ = command_tx.send(command).await;
                            }
                        }
                        None => break,
                    }
                }
                Some((id, event)) = event_rx.recv() => {
                    let disconnected = matches!(event, SessionEvent::Disconnected { .. });
                    if disconnected {
                        sessions.remove(&id);
                    }
                    if output.send(Message::SessionEvent(id, event)).await.is_err() {
                        break;
                    }
                }
            }
        }
    })
}

/// Sends `command` to session `id` via the SSH worker, if it's ready and the
/// session is still known to it. Silently drops the command otherwise (the
/// worker itself will already have reported the disconnect as an event).
fn send_to_session(app: &TermiteApp, id: SessionId, command: SessionCommand) {
    if let Some(sender) = &app.ssh_worker {
        let mut sender = sender.clone();
        let _ = sender.try_send(SshWorkerInput::Send(id, command));
    }
}

// ── Messages ──────────────────────────────────────────────────────────────────

/// All messages that flow through the Iced update loop.
///
/// Extended in M1+ as features are introduced.
#[derive(Debug, Clone)]
pub enum Message {
    PollOutput,
    KeyPressed {
        key: Key,
        modifiers: Modifiers,
    },
    HostsLoaded(Vec<HostProfile>),
    Sidebar(SidebarMessage),
    /// The SSH worker subscription is up; this is the channel to send it
    /// [`SshWorkerInput`] on.
    SshWorkerReady(bridge::Sender<SshWorkerInput>),
    /// An event from a running SSH session, forwarded by the worker.
    SessionEvent(SessionId, SessionEvent),
    /// Interaction with the pending credential/host-key modal, if any.
    Prompt(PromptMessage),

    // ── M5: tabs ────────────────────────────────────────────────────
    /// The worker finished spawning the session requested for this tab and
    /// assigned it this id (see [`SshWorkerInput::Connect`]).
    SessionSpawned(TabId, SessionId),
    /// A scheduled automatic-reconnect backoff for this tab has elapsed.
    /// No-op if the tab was closed in the meantime.
    AttemptReconnect(TabId),
    /// Opens a new local shell tab.
    NewLocalTab,
    /// Interaction with the tab bar.
    TabBar(TabBarMessage),

    // ── M6: advanced terminal features ────────────────────────────────
    /// The window was resized; carries its new size in pixels (see
    /// `grid_size_for_window`).
    WindowResized(iced::Size),
    /// A tab's bell-flash display window (`BELL_FLASH_DURATION`) elapsed.
    /// No-op if the tab was closed in the meantime.
    BellTimeout(TabId),
    /// The system clipboard was read in response to a paste shortcut;
    /// `None` if the clipboard was empty or unreadable.
    Pasted(Option<String>),
    /// The cursor moved within the terminal pane, at this position local to
    /// the pane's `mouse_area`.
    MouseMoved(iced::Point),
    /// A mouse button went down over the terminal pane.
    MousePress(iced::mouse::Button),
    /// A mouse button was released over the terminal pane.
    MouseRelease(iced::mouse::Button),
    /// The wheel was scrolled over the terminal pane.
    MouseScrolled(iced::mouse::ScrollDelta),
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Initialise logging and launch the Iced application.
pub fn run() -> iced::Result {
    init_tracing();

    iced::application("Termite", update, view)
        .theme(|_| Theme::Dark)
        .subscription(subscription)
        .run_with(initialize)
}

fn initialize() -> (TermiteApp, Task<Message>) {
    let app = TermiteApp::new().expect("failed to spawn local shell pty");
    let load = load_hosts_task(Arc::clone(&app.host_store));
    (app, load)
}

// ── Iced functions ────────────────────────────────────────────────────────────

fn update(app: &mut TermiteApp, message: Message) -> Task<Message> {
    match message {
        Message::PollOutput => {
            let mut rung = Vec::new();
            for tab in &mut app.tabs {
                let bytes = match &tab.kind {
                    TabKind::Local { output, .. } => match output.lock() {
                        Ok(mut output) => std::mem::take(&mut *output),
                        Err(_) => Vec::new(),
                    },
                    TabKind::Ssh { .. } => Vec::new(),
                };
                if !bytes.is_empty() && tab.advance(&bytes) {
                    tab.bell_flash = true;
                    rung.push(tab.id);
                }
            }
            if !rung.is_empty() {
                return Task::batch(rung.into_iter().map(bell_timeout_task));
            }
        }
        Message::KeyPressed { key, modifiers } => {
            if is_paste_shortcut(&key, modifiers) {
                return iced::clipboard::read().map(Message::Pasted);
            }
            if let Some(task) = handle_tab_shortcut(app, &key, modifiers) {
                return task;
            }
            if let Some(bytes) = key_to_bytes(&key, modifiers) {
                let ssh_target = match app.active_tab_mut() {
                    Some(tab) => match &mut tab.kind {
                        TabKind::Local { writer, .. } => {
                            let _ = writer.write_all(&bytes);
                            None
                        }
                        TabKind::Ssh { session_id, .. } => *session_id,
                    },
                    None => None,
                };
                if let Some(id) = ssh_target {
                    send_to_session(app, id, SessionCommand::Write(bytes));
                }
            }
        }
        Message::HostsLoaded(mut hosts) => {
            sort_hosts(&mut hosts);
            app.hosts = hosts;
        }
        Message::Sidebar(message) => return update_sidebar(app, message),
        Message::SshWorkerReady(sender) => {
            app.ssh_worker = Some(sender);
        }
        Message::SessionEvent(id, event) => return handle_session_event(app, id, event),
        Message::Prompt(message) => update_prompt(app, message),
        Message::SessionSpawned(tab_id, session_id) => {
            if let Some(tab) = app.find_tab_mut(tab_id) {
                if let TabKind::Ssh {
                    session_id: slot, ..
                } = &mut tab.kind
                {
                    *slot = Some(session_id);
                }
            }
        }
        Message::AttemptReconnect(tab_id) => return reconnect(app, tab_id),
        Message::NewLocalTab => new_local_tab(app),
        Message::TabBar(message) => return update_tabbar(app, message),
        Message::WindowResized(size) => {
            let (rows, cols) = grid_size_for_window(size);
            app.grid_size = (rows, cols);
            resize_all_tabs(app, rows, cols);
        }
        Message::BellTimeout(tab_id) => {
            if let Some(tab) = app.find_tab_mut(tab_id) {
                tab.bell_flash = false;
            }
        }
        Message::Pasted(Some(text)) => paste_into_active_tab(app, &text),
        Message::Pasted(None) => {}
        Message::MouseMoved(point) => {
            app.mouse_position = Some(point);
            report_mouse_event(app, point, MouseReportKind::Motion);
            if app.mouse_button_down == Some(iced::mouse::Button::Left) {
                update_selection_head(app, point);
            }
        }
        Message::MousePress(button) => {
            app.mouse_button_down = Some(button);
            if let Some(point) = app.mouse_position {
                report_mouse_event(app, point, MouseReportKind::Press(button));
                if button == iced::mouse::Button::Left {
                    start_selection(app, point);
                }
            }
        }
        Message::MouseRelease(button) => {
            app.mouse_button_down = None;
            if let Some(point) = app.mouse_position {
                report_mouse_event(app, point, MouseReportKind::Release(button));
            }
            if button == iced::mouse::Button::Left {
                return finish_selection(app);
            }
        }
        Message::MouseScrolled(delta) => {
            if let Some(point) = app.mouse_position {
                let y = match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. }
                    | iced::mouse::ScrollDelta::Pixels { y, .. } => y,
                };
                let kind = if y > 0.0 {
                    MouseReportKind::WheelUp
                } else {
                    MouseReportKind::WheelDown
                };
                report_mouse_event(app, point, kind);
            }
        }
    }
    Task::none()
}

/// Translates a window size in pixels into a row/column grid size, using
/// the approximate cell metrics and fixed chrome sizes defined above.
/// Always at least 1x1, so a tiny or not-yet-laid-out window can't produce
/// a zero-size grid.
fn grid_size_for_window(size: iced::Size) -> (usize, usize) {
    let cols = ((size.width - SIDEBAR_WIDTH_PX) / CELL_WIDTH_PX).floor();
    let rows = ((size.height - CHROME_HEIGHT_PX) / CELL_HEIGHT_PX).floor();
    (rows.max(1.0) as usize, cols.max(1.0) as usize)
}

/// Resizes every open tab's grid to `rows`/`cols` and propagates the new
/// size to whatever's feeding it: the local pty, or (for a live SSH tab) a
/// `SessionCommand::Resize` forwarded to the remote pty.
fn resize_all_tabs(app: &mut TermiteApp, rows: usize, cols: usize) {
    let mut ssh_targets = Vec::new();
    for tab in &mut app.tabs {
        tab.grid.resize(rows, cols);
        match &tab.kind {
            TabKind::Local { pty, .. } => {
                if let Err(err) = pty.resize(rows as u16, cols as u16) {
                    tracing::warn!(%err, "failed to resize local pty");
                }
            }
            TabKind::Ssh { session_id, .. } => {
                if let Some(id) = session_id {
                    ssh_targets.push(*id);
                }
            }
        }
    }
    for id in ssh_targets {
        send_to_session(
            app,
            id,
            SessionCommand::Resize {
                rows: rows as u16,
                cols: cols as u16,
            },
        );
    }
}

/// Schedules `Message::BellTimeout(tab_id)` after `BELL_FLASH_DURATION`.
/// `sleep` is constructed inside the async block (not passed directly to
/// `Task::perform`) so it isn't registered with the Tokio timer driver
/// until the future is actually polled by a real runtime — see v14's
/// reconnect-backoff note in `HANDOFF.md` for why the eager form panics
/// under a plain `#[test]`.
fn bell_timeout_task(tab_id: TabId) -> Task<Message> {
    Task::perform(
        async move { tokio::time::sleep(BELL_FLASH_DURATION).await },
        move |_| Message::BellTimeout(tab_id),
    )
}

/// Whether this key press is the paste shortcut (`Ctrl+Shift+V`). Plain
/// `Ctrl+V` is deliberately left alone — in `key_to_bytes` it already
/// forwards as the control byte `0x16`, which readline and friends treat as
/// "quoted insert"; claiming it for paste would be a behavior regression
/// the way claiming `Ctrl+Tab`/`Ctrl+<digit>` for tab navigation wasn't
/// (see `handle_tab_shortcut`).
fn is_paste_shortcut(key: &Key, modifiers: Modifiers) -> bool {
    modifiers.control()
        && modifiers.shift()
        && matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("v"))
}

/// Sends pasted clipboard text to the active tab, wrapped in
/// `ESC[200~`/`ESC[201~` if the tab's grid has bracketed paste mode
/// enabled (DEC private mode 2004). A no-op if there's no active tab.
fn paste_into_active_tab(app: &mut TermiteApp, text: &str) {
    let Some(tab) = app.active_tab_mut() else {
        return;
    };
    let mut bytes = Vec::new();
    let bracketed = tab.grid.bracketed_paste();
    if bracketed {
        bytes.extend_from_slice(b"\x1b[200~");
    }
    bytes.extend_from_slice(text.as_bytes());
    if bracketed {
        bytes.extend_from_slice(b"\x1b[201~");
    }

    let ssh_target = match &mut tab.kind {
        TabKind::Local { writer, .. } => {
            let _ = writer.write_all(&bytes);
            None
        }
        TabKind::Ssh { session_id, .. } => *session_id,
    };
    if let Some(id) = ssh_target {
        send_to_session(app, id, SessionCommand::Write(bytes));
    }
}

// ── Mouse reporting (xterm mouse protocol) ─────────────────────────────────────

/// What happened, before it's turned into an xterm mouse-report code.
#[derive(Debug, Clone, Copy)]
enum MouseReportKind {
    Press(iced::mouse::Button),
    Release(iced::mouse::Button),
    /// Cursor moved; which (if any) button was held is read from
    /// `TermiteApp::mouse_button_down` by `report_mouse_event` rather than
    /// carried here, since it's app state, not part of the move event
    /// itself.
    Motion,
    WheelUp,
    WheelDown,
}

/// Maps a left/middle/right button to its xterm base code. Back/Forward
/// (and any future variants) have no xterm equivalent and are dropped —
/// `report_mouse_event` treats that as "nothing to report".
fn mouse_button_code(button: iced::mouse::Button) -> Option<u8> {
    match button {
        iced::mouse::Button::Left => Some(0),
        iced::mouse::Button::Middle => Some(1),
        iced::mouse::Button::Right => Some(2),
        _ => None,
    }
}

/// Converts a pane-local pixel position into a 1-indexed (col, row) cell,
/// clamped to the grid's current bounds. Uses the same approximate cell
/// metrics as `grid_size_for_window` — see that function's doc comment for
/// why they're not measured against the real font.
fn cell_at(point: iced::Point, rows: usize, cols: usize) -> (usize, usize) {
    let col = (point.x / CELL_WIDTH_PX).floor() as isize + 1;
    let row = (point.y / CELL_HEIGHT_PX).floor() as isize + 1;
    (
        col.clamp(1, cols as isize) as usize,
        row.clamp(1, rows as isize) as usize,
    )
}

/// Encodes one mouse report. `code` is the xterm base button code (0/1/2
/// for left/middle/right, `MOUSE_NO_BUTTON` for a buttonless motion report,
/// `MOUSE_WHEEL_UP`/`MOUSE_WHEEL_DOWN` for the wheel, with
/// `MOUSE_MOTION_FLAG` already added by the caller for a motion report).
/// `col`/`row` are 1-indexed.
///
/// SGR mode (`?1006`) reports `ESC[<code;col;rowM` for a press/motion and
/// `...m` for a release, with no coordinate limit. Legacy mode reports the
/// fixed 6-byte `ESC[M CbCxCy` form, everything offset by 32 to stay
/// printable — which caps any reportable coordinate at 223 (255 - 32) and
/// can't distinguish *which* button was released, only that one was (code
/// `MOUSE_NO_BUTTON`), so `release` is ignored for `code`'s value there and
/// only changes which fixed code legacy mode sends.
fn encode_mouse_event(code: u8, release: bool, col: usize, row: usize, sgr: bool) -> Vec<u8> {
    if sgr {
        let mut out = format!("\x1b[<{code};{col};{row}").into_bytes();
        out.push(if release { b'm' } else { b'M' });
        out
    } else {
        let clamp_coord = |v: usize| (v.min(223) as u8) + 32;
        let cb = if release { MOUSE_NO_BUTTON } else { code };
        vec![
            0x1b,
            b'[',
            b'M',
            cb + 32,
            clamp_coord(col),
            clamp_coord(row),
        ]
    }
}

/// Reports a mouse event to the active tab's session, if its grid currently
/// has mouse tracking enabled and (for a motion report) the active tracking
/// mode actually covers this kind of motion. A no-op otherwise — including
/// when there's no active tab, or the event is a press/release of a button
/// xterm has no code for (`mouse_button_code` returns `None`).
fn report_mouse_event(app: &mut TermiteApp, point: iced::Point, kind: MouseReportKind) {
    let held = app.mouse_button_down;
    let Some(tab) = app.active_tab_mut() else {
        return;
    };
    let tracking = tab.grid.mouse_tracking();
    if tracking == MouseTracking::Off {
        return;
    }
    if matches!(kind, MouseReportKind::Motion) {
        let covered = matches!(tracking, MouseTracking::AnyEvent)
            || (matches!(tracking, MouseTracking::ButtonEvent) && held.is_some());
        if !covered {
            return;
        }
    }

    let sgr = tab.grid.mouse_sgr();
    let (col, row) = cell_at(point, tab.grid.rows(), tab.grid.cols());
    let bytes = match kind {
        MouseReportKind::Press(button) => {
            mouse_button_code(button).map(|code| encode_mouse_event(code, false, col, row, sgr))
        }
        MouseReportKind::Release(button) => {
            mouse_button_code(button).map(|code| encode_mouse_event(code, true, col, row, sgr))
        }
        MouseReportKind::Motion => {
            let code =
                held.and_then(mouse_button_code).unwrap_or(MOUSE_NO_BUTTON) + MOUSE_MOTION_FLAG;
            Some(encode_mouse_event(code, false, col, row, sgr))
        }
        MouseReportKind::WheelUp => Some(encode_mouse_event(MOUSE_WHEEL_UP, false, col, row, sgr)),
        MouseReportKind::WheelDown => {
            Some(encode_mouse_event(MOUSE_WHEEL_DOWN, false, col, row, sgr))
        }
    };
    let Some(bytes) = bytes else {
        return;
    };

    let ssh_target = match &mut tab.kind {
        TabKind::Local { writer, .. } => {
            let _ = writer.write_all(&bytes);
            None
        }
        TabKind::Ssh { session_id, .. } => *session_id,
    };
    if let Some(id) = ssh_target {
        send_to_session(app, id, SessionCommand::Write(bytes));
    }
}

// ── Text selection ───────────────────────────────────────────────────────────

/// Converts a pane-local pixel position into a 0-indexed `(row, col)` grid
/// cell, clamped to the grid's current bounds. Selection tracks cells
/// 0-indexed (matching `TerminalGrid`/`visible_rows` indexing) rather than
/// the 1-indexed form `cell_at` returns for xterm reports, so this just
/// reuses `cell_at`'s pixel math and shifts it down by one.
fn cell_at_zero_indexed(point: iced::Point, rows: usize, cols: usize) -> (usize, usize) {
    let (col, row) = cell_at(point, rows, cols);
    (row - 1, col - 1)
}

/// Determines the `SelectionMode` a left-click at `cell` should start,
/// cycling character → word → line on repeated clicks landing on the same
/// cell within `DOUBLE_CLICK_WINDOW` — the usual
/// click/double-click/triple-click convention — and updates
/// `TermiteApp::last_click` to chain a further click after this one.
fn click_selection_mode(app: &mut TermiteApp, cell: (usize, usize)) -> SelectionMode {
    let now = std::time::Instant::now();
    let count = match app.last_click {
        Some((at, at_cell, count))
            if at_cell == cell && now.duration_since(at) < DOUBLE_CLICK_WINDOW =>
        {
            (count % 3) + 1
        }
        _ => 1,
    };
    app.last_click = Some((now, cell, count));
    match count {
        1 => SelectionMode::Character,
        2 => SelectionMode::Word,
        _ => SelectionMode::Line,
    }
}

/// Starts a local text-selection gesture at `point` on a left-button press.
/// A no-op — and clears any existing selection — when the active tab's grid
/// currently has xterm mouse tracking enabled, since that app already owns
/// click/drag (see `report_mouse_event`); there's no modifier-key override
/// yet, as `mouse_area`'s callbacks don't surface `Modifiers` to check for
/// one (same gap noted on `report_mouse_event`).
fn start_selection(app: &mut TermiteApp, point: iced::Point) {
    let Some((rows, cols, tracking)) = app
        .active_tab()
        .map(|tab| (tab.grid.rows(), tab.grid.cols(), tab.grid.mouse_tracking()))
    else {
        return;
    };
    let cell = cell_at_zero_indexed(point, rows, cols);
    if tracking != MouseTracking::Off {
        if let Some(tab) = app.active_tab_mut() {
            tab.selection = None;
        }
        return;
    }
    let mode = click_selection_mode(app, cell);
    if let Some(tab) = app.active_tab_mut() {
        tab.selection = Some(Selection {
            anchor: cell,
            head: cell,
            mode,
        });
    }
}

/// Extends the active tab's in-progress selection to `point`, while the left
/// button is held (see `Message::MouseMoved`). A no-op if there's no
/// in-progress selection (e.g. mouse tracking claimed the gesture instead).
fn update_selection_head(app: &mut TermiteApp, point: iced::Point) {
    let Some(tab) = app.active_tab_mut() else {
        return;
    };
    let (rows, cols) = (tab.grid.rows(), tab.grid.cols());
    let cell = cell_at_zero_indexed(point, rows, cols);
    if let Some(selection) = tab.selection.as_mut() {
        selection.head = cell;
    }
}

/// Finalizes a left-button release. A plain click with no drag in
/// `SelectionMode::Character` (anchor never moved) selects nothing useful,
/// so it's dropped rather than left showing a zero-width highlight; a
/// double/triple click's word/line selection is kept even without a drag,
/// since it's already meaningful on its own. Whatever's left standing is
/// copied to the system clipboard — matching common terminal "select to
/// copy" behavior — via a returned `Task`; `Task::none()` if there's nothing
/// to copy.
fn finish_selection(app: &mut TermiteApp) -> Task<Message> {
    let Some(tab) = app.active_tab_mut() else {
        return Task::none();
    };
    let Some(selection) = tab.selection else {
        return Task::none();
    };
    if selection.mode == SelectionMode::Character && selection.anchor == selection.head {
        tab.selection = None;
        return Task::none();
    }

    let rows = grid_char_rows(&tab.grid);
    let text = selection_text(&rows, &selection);
    if text.is_empty() {
        return Task::none();
    }
    iced::clipboard::write(text)
}

/// Splits a grid's visible rows into per-character `Vec<char>`s, the shape
/// `selection_range`/`selection_text`/`terminal_spans` all work in — avoids
/// byte-index slicing on `String`, which would panic on any non-ASCII cell.
fn grid_char_rows(grid: &TerminalGrid) -> Vec<Vec<char>> {
    grid.visible_rows()
        .iter()
        .map(|row| row.chars().collect())
        .collect()
}

/// Expands `col` on `row` to the start/end of the run of same-"wordness"
/// characters it sits in (`is_alphanumeric` or `_` vs. everything else,
/// including whitespace and punctuation, which are their own run) — the
/// usual double-click word-selection rule.
fn word_bounds(row: &[char], col: usize) -> (usize, usize) {
    if row.is_empty() {
        return (0, 0);
    }
    let col = col.min(row.len() - 1);
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let class = is_word(row[col]);
    let mut start = col;
    while start > 0 && is_word(row[start - 1]) == class {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < row.len() && is_word(row[end + 1]) == class {
        end += 1;
    }
    (start, end)
}

/// Normalizes a `Selection`'s `anchor`/`head` (which may point in either
/// reading-order direction) into an inclusive `(start, end)` `(row, col)`
/// range, expanded per `SelectionMode`: `Character` uses the raw range,
/// `Word` expands both ends to their word boundaries (`word_bounds`), `Line`
/// expands to the full width of every covered row. Used for both rendering
/// the highlight (`terminal_spans`) and extracting the copied text
/// (`selection_text`).
fn selection_range(rows: &[Vec<char>], selection: &Selection) -> ((usize, usize), (usize, usize)) {
    let (mut start, mut end) = if selection.anchor <= selection.head {
        (selection.anchor, selection.head)
    } else {
        (selection.head, selection.anchor)
    };
    match selection.mode {
        SelectionMode::Character => {}
        SelectionMode::Word => {
            if let Some(row) = rows.get(start.0) {
                start.1 = word_bounds(row, start.1).0;
            }
            if let Some(row) = rows.get(end.0) {
                end.1 = word_bounds(row, end.1).1;
            }
        }
        SelectionMode::Line => {
            start.1 = 0;
            end.1 = rows.get(end.0).map_or(0, |row| row.len().saturating_sub(1));
        }
    }
    (start, end)
}

/// Extracts the plain text covered by `selection`, one grid row per line,
/// each row's trailing padding (grid rows are always padded to the full
/// column width with spaces) trimmed so a copy doesn't end every line with a
/// wall of whitespace.
fn selection_text(rows: &[Vec<char>], selection: &Selection) -> String {
    let (start, end) = selection_range(rows, selection);
    let mut lines = Vec::with_capacity(end.0 - start.0 + 1);
    for r in start.0..=end.0 {
        let row = rows.get(r).map_or(&[][..], Vec::as_slice);
        let lo = if r == start.0 {
            start.1.min(row.len())
        } else {
            0
        };
        let hi = if r == end.0 {
            (end.1 + 1).min(row.len())
        } else {
            row.len()
        };
        lines.push(
            row[lo..hi]
                .iter()
                .collect::<String>()
                .trim_end()
                .to_string(),
        );
    }
    lines.join("\n")
}

/// Builds the `rich_text` spans for a tab's terminal pane: the row text as
/// plain spans, with whatever `selection` covers pulled out into its own
/// span carrying a `SELECTION`-coloured background. Rows are always full
/// grid width (padded with spaces), so column indices from
/// `selection_range` never need clamping against a shorter row.
fn terminal_spans(
    rows: &[Vec<char>],
    selection: Option<&Selection>,
) -> Vec<Span<'static, Message>> {
    let range = selection.map(|selection| selection_range(rows, selection));

    let mut spans = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        if r > 0 {
            spans.push(Span::new("\n".to_string()));
        }
        let covers_this_row = range.filter(|(start, end)| r >= start.0 && r <= end.0);
        match covers_this_row {
            Some((start, end)) => {
                let sel_start = if r == start.0 { start.1 } else { 0 };
                let sel_end = if r == end.0 {
                    end.1
                } else {
                    row.len().saturating_sub(1)
                };
                push_row_with_selection(&mut spans, row, sel_start, sel_end);
            }
            None => spans.push(Span::new(row.iter().collect::<String>())),
        }
    }
    spans
}

/// Pushes up to three spans for one row: the unselected prefix (if any), the
/// selected run with a `SELECTION` background, and the unselected suffix (if
/// any). `sel_start`/`sel_end` are inclusive and clamped to the row.
fn push_row_with_selection(
    spans: &mut Vec<Span<'static, Message>>,
    row: &[char],
    sel_start: usize,
    sel_end: usize,
) {
    if row.is_empty() {
        return;
    }
    let sel_start = sel_start.min(row.len() - 1);
    let sel_end = sel_end.min(row.len() - 1);
    if sel_start > 0 {
        spans.push(Span::new(row[..sel_start].iter().collect::<String>()));
    }
    spans.push(
        Span::new(row[sel_start..=sel_end].iter().collect::<String>())
            .background(termite_ui::theme::colours::SELECTION),
    );
    if sel_end + 1 < row.len() {
        spans.push(Span::new(row[sel_end + 1..].iter().collect::<String>()));
    }
}

/// Handles interaction with the tab bar.
fn update_tabbar(app: &mut TermiteApp, message: TabBarMessage) -> Task<Message> {
    match message {
        TabBarMessage::Select(id) => {
            if app.find_tab(id).is_some() {
                app.active_tab = Some(id);
            }
        }
        TabBarMessage::Close(id) => return close_tab(app, id),
        TabBarMessage::Retry(id) => return reconnect(app, id),
        TabBarMessage::NewLocal => new_local_tab(app),
    }
    Task::none()
}

/// Removes tab `id`. Disconnects its live SSH session (if any) and clears
/// any prompt waiting on it. If it was the active tab, activates a
/// neighbor; if it was the only tab, immediately opens a fresh local one —
/// `TermiteApp::tabs` is never left empty.
fn close_tab(app: &mut TermiteApp, id: TabId) -> Task<Message> {
    let Some(index) = app.tabs.iter().position(|tab| tab.id == id) else {
        return Task::none();
    };
    let tab = app.tabs.remove(index);

    if let Some(session_id) = tab.kind.session_id() {
        send_to_session(app, session_id, SessionCommand::Disconnect);
    }
    if pending_prompt_session(&app.pending_prompt) == tab.kind.session_id() {
        app.pending_prompt = None;
    }

    if app.active_tab == Some(id) {
        app.active_tab = app
            .tabs
            .get(index.min(app.tabs.len().saturating_sub(1)))
            .map(|tab| tab.id);
    }

    if app.tabs.is_empty() {
        new_local_tab(app);
    }
    Task::none()
}

/// The exponential-with-cap delay before automatic reconnect attempt
/// `attempt` (1-indexed): 2s, 4s, 8s, 16s, then capped at 30s.
fn backoff_delay(attempt: u32) -> Duration {
    let secs = 2u64.saturating_pow(attempt.min(5));
    Duration::from_secs(secs.min(30))
}

/// (Re)connects tab `id` using its stored `HostProfile`, whether that's the
/// first connection attempt, a scheduled automatic retry
/// (`Message::AttemptReconnect`), or a user-initiated one
/// (`TabBarMessage::Retry`). A no-op if the tab was closed in the meantime,
/// isn't SSH-backed, or the worker isn't ready yet.
fn reconnect(app: &mut TermiteApp, id: TabId) -> Task<Message> {
    let Some(tab) = app.find_tab_mut(id) else {
        return Task::none();
    };
    let TabKind::Ssh { profile, .. } = &tab.kind else {
        return Task::none();
    };
    let profile = profile.clone();
    tab.status = ConnectionStatus::Connecting;
    match &app.ssh_worker {
        Some(sender) => {
            let mut sender = sender.clone();
            let _ = sender.try_send(SshWorkerInput::Connect(id, profile));
        }
        None => tracing::warn!("ssh worker not ready yet; reconnect dropped"),
    }
    Task::none()
}

/// Intercepts Ctrl-held tab-navigation keys before they'd otherwise be
/// forwarded as bytes to the active tab. Returns `None` for anything that
/// isn't a tab shortcut, so the caller falls through to normal handling.
/// (Today, unintercepted, Ctrl+Tab sends a literal tab byte and Ctrl+<digit>
/// sends the bare digit — neither is a meaningful terminal control
/// sequence, so claiming them here isn't a behavior regression.)
fn handle_tab_shortcut(
    app: &mut TermiteApp,
    key: &Key,
    modifiers: Modifiers,
) -> Option<Task<Message>> {
    if !modifiers.control() {
        return None;
    }
    match key {
        Key::Named(Named::Tab) => {
            select_adjacent_tab(app, if modifiers.shift() { -1 } else { 1 });
            Some(Task::none())
        }
        Key::Character(c) => {
            let digit = c.chars().next()?.to_digit(10)?;
            if (1..=9).contains(&digit) {
                select_tab_by_index(app, digit as usize - 1);
                Some(Task::none())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Moves `active_tab` forward (`delta = 1`) or backward (`delta = -1`),
/// wrapping around. A no-op if there's no active tab (shouldn't happen
/// outside tests, since `tabs` is never empty) or only one tab open.
fn select_adjacent_tab(app: &mut TermiteApp, delta: isize) {
    if app.tabs.len() < 2 {
        return;
    }
    let Some(active) = app.active_tab else {
        return;
    };
    let Some(current) = app.tabs.iter().position(|tab| tab.id == active) else {
        return;
    };
    let len = app.tabs.len() as isize;
    let next = ((current as isize + delta).rem_euclid(len)) as usize;
    app.active_tab = Some(app.tabs[next].id);
}

/// Selects the tab at `index` (0-based), if one exists at that position.
fn select_tab_by_index(app: &mut TermiteApp, index: usize) {
    if let Some(tab) = app.tabs.get(index) {
        app.active_tab = Some(tab.id);
    }
}

/// The display data the tab bar renders, in `app.tabs`' order.
fn tab_summaries(app: &TermiteApp) -> Vec<TabSummary> {
    app.tabs
        .iter()
        .map(|tab| TabSummary {
            id: tab.id,
            title: tab.title.clone(),
            status: tab.status.clone(),
        })
        .collect()
}

/// Handles an event forwarded from a running SSH session, routed to
/// whichever tab owns `id` (see `find_tab_by_session_mut`) — a background
/// tab keeps accumulating its own scrollback even while another tab is
/// active, rather than the pre-M5 behavior of only rendering whatever
/// session happened to be "active". Connection lifecycle transitions are
/// also appended to that tab's grid as plain text alongside the tab bar's
/// status indicator, since it's the more detailed of the two surfaces.
///
/// `AuthRequired` and `HostKeyUnknown`/`HostKeyMismatch` open the credential
/// or host-key modal (see `update_prompt`) and focus the owning tab so the
/// modal is seen in context. Per `CLAUDE.md`'s no-silent-accept invariant,
/// only one prompt is shown at a time: if a second one arrives while the
/// modal is already open, it fails closed (auth disconnects, host-key
/// rejects) rather than silently overwriting the pending decision.
///
/// `Disconnected` with anything other than a user-requested reason schedules
/// an automatic reconnect with backoff (see `backoff_delay`), up to
/// `MAX_RECONNECT_ATTEMPTS`, via the `Task` this returns.
fn handle_session_event(app: &mut TermiteApp, id: SessionId, event: SessionEvent) -> Task<Message> {
    match event {
        SessionEvent::Connected => {
            if let Some(tab) = app.find_tab_by_session_mut(id) {
                tab.status = ConnectionStatus::Connected;
                if let TabKind::Ssh {
                    reconnect_attempt, ..
                } = &mut tab.kind
                {
                    *reconnect_attempt = 0;
                }
                tab.advance(b"\r\n*** connected ***\r\n");
            }
        }
        SessionEvent::Output(bytes) => {
            if let Some(tab) = app.find_tab_by_session_mut(id) {
                if tab.advance(&bytes) {
                    tab.bell_flash = true;
                    return bell_timeout_task(tab.id);
                }
            }
        }
        SessionEvent::AuthRequired(challenge) => {
            if app.pending_prompt.is_some() {
                tracing::warn!(
                    ?challenge,
                    "a prompt is already pending; disconnecting session"
                );
                send_to_session(app, id, SessionCommand::Disconnect);
                return Task::none();
            }
            if let Some(secret) = saved_credential(app, &challenge) {
                let response = match &challenge {
                    AuthChallenge::Password { .. } => AuthResponse::Password(secret),
                    AuthChallenge::Passphrase { .. } => AuthResponse::Passphrase(secret),
                };
                send_to_session(app, id, SessionCommand::AuthResponse(response));
                return Task::none();
            }
            focus_tab_for_session(app, id);
            let label = match &challenge {
                AuthChallenge::Password { .. } => {
                    "Password required to continue connecting".to_string()
                }
                AuthChallenge::Passphrase { fingerprint } => {
                    format!("Passphrase for key {fingerprint}")
                }
            };
            app.pending_prompt = Some(PendingPrompt::Credential {
                session: id,
                challenge,
                ui: Prompt::Credential {
                    label,
                    input: String::new(),
                    save: false,
                },
            });
        }
        SessionEvent::HostKeyUnknown(key) => {
            open_host_key_prompt(app, id, key, false);
        }
        SessionEvent::HostKeyMismatch(key) => {
            open_host_key_prompt(app, id, key, true);
        }
        SessionEvent::Disconnected { reason } => {
            tracing::info!(?reason, "ssh session disconnected");
            if pending_prompt_session(&app.pending_prompt) == Some(id) {
                app.pending_prompt = None;
            }
            if let Some(tab) = app.find_tab_by_session_mut(id) {
                tab.advance(format!("\r\n*** disconnected: {reason:?} ***\r\n").as_bytes());
                let tab_id = tab.id;
                let (should_retry, attempt) = match &mut tab.kind {
                    TabKind::Ssh {
                        session_id,
                        reconnect_attempt,
                        ..
                    } => {
                        *session_id = None;
                        let should_retry = !matches!(reason, DisconnectReason::Requested)
                            && *reconnect_attempt < MAX_RECONNECT_ATTEMPTS;
                        if should_retry {
                            *reconnect_attempt += 1;
                        }
                        (should_retry, *reconnect_attempt)
                    }
                    TabKind::Local { .. } => (false, 0),
                };
                tab.status = if should_retry {
                    ConnectionStatus::Reconnecting { attempt }
                } else {
                    ConnectionStatus::Disconnected
                };
                if should_retry {
                    // The `sleep` future is built *inside* the async block
                    // rather than passed to `Task::perform` directly:
                    // `tokio::time::sleep` registers with the runtime's timer
                    // driver as soon as it's constructed, which panics
                    // outside a Tokio runtime context (e.g. in a plain
                    // `#[test]` fn that never polls this `Task` at all).
                    // Deferring construction until the future is actually
                    // polled avoids that.
                    let delay = backoff_delay(attempt);
                    return Task::perform(
                        async move { tokio::time::sleep(delay).await },
                        move |_| Message::AttemptReconnect(tab_id),
                    );
                }
            }
        }
        SessionEvent::Error(message) => {
            tracing::error!(%message, "ssh session error");
            if let Some(tab) = app.find_tab_by_session_mut(id) {
                tab.advance(format!("\r\n*** error: {message} ***\r\n").as_bytes());
            }
        }
    }
    Task::none()
}

/// Switches `active_tab` to whichever tab owns SSH session `id`, if any —
/// used when a prompt opens for a session that isn't currently focused.
fn focus_tab_for_session(app: &mut TermiteApp, id: SessionId) {
    if let Some(tab_id) = app.find_tab_by_session_mut(id).map(|tab| tab.id) {
        app.active_tab = Some(tab_id);
    }
}

/// Looks up a previously-saved credential for `challenge` in the keychain
/// (see `save_credential`, called from `update_prompt` on `Submit`). Lookup
/// failures (e.g. no keychain daemon running) are treated the same as "not
/// found" — falling back to prompting the user is always safe, unlike
/// silently failing a connection over a keychain hiccup.
fn saved_credential(app: &TermiteApp, challenge: &AuthChallenge) -> Option<SecretString> {
    let result = match challenge {
        AuthChallenge::Password { host, username } => {
            app.credential_store.get_password(host, username)
        }
        AuthChallenge::Passphrase { fingerprint } => {
            app.credential_store.get_passphrase(fingerprint)
        }
    };
    match result {
        Ok(secret) => secret,
        Err(err) => {
            tracing::warn!(%err, "credential store lookup failed; falling back to prompt");
            None
        }
    }
}

/// Saves `secret` to the keychain under the key `challenge` implies, when
/// the user has checked the prompt's "Save to keychain" toggle.
fn save_credential(
    app: &TermiteApp,
    challenge: &AuthChallenge,
    secret: &SecretString,
) -> Result<(), TermiteError> {
    match challenge {
        AuthChallenge::Password { host, username } => {
            app.credential_store.set_password(host, username, secret)
        }
        AuthChallenge::Passphrase { fingerprint } => {
            app.credential_store.set_passphrase(fingerprint, secret)
        }
    }
}

/// Removes any keychain entry associated with `profile`'s auth method, so
/// deleting a host doesn't leave a stale credential behind in the keychain.
/// Best-effort: a lookup/delete failure (key file already gone, keychain
/// hiccup) is logged and otherwise ignored — it must never block the host
/// deletion itself, which is why this isn't threaded through the async
/// `HostStore::delete` task the caller returns.
fn forget_credential(store: &Arc<dyn CredentialStore>, profile: &HostProfile) {
    match &profile.auth {
        AuthMethod::Agent => {}
        AuthMethod::Password => {
            if let Err(err) = store.delete_password(&profile.host, &profile.username) {
                tracing::warn!(%err, "failed to delete saved password for deleted host");
            }
        }
        AuthMethod::PublicKey { key_path } => match termite_crypto::key::load(key_path) {
            Ok(key) => {
                let fingerprint = termite_crypto::key::fingerprint(&key);
                if let Err(err) = store.delete_passphrase(&fingerprint) {
                    tracing::warn!(%err, "failed to delete saved passphrase for deleted host");
                }
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    "failed to load key file to determine passphrase fingerprint; \
                     any saved passphrase for this host is left in the keychain"
                );
            }
        },
    }
}

/// The session a pending prompt (if any) belongs to.
fn pending_prompt_session(pending: &Option<PendingPrompt>) -> Option<SessionId> {
    match pending {
        Some(PendingPrompt::Credential { session, .. }) => Some(*session),
        Some(PendingPrompt::HostKey { session, .. }) => Some(*session),
        None => None,
    }
}

/// Opens the host-key approval modal, or fails closed by rejecting the key
/// if a different prompt is already pending (see `handle_session_event`).
fn open_host_key_prompt(app: &mut TermiteApp, session: SessionId, key: HostKey, mismatch: bool) {
    if app.pending_prompt.is_some() {
        tracing::warn!(
            ?key,
            mismatch,
            "a prompt is already pending; rejecting for safety"
        );
        send_to_session(app, session, SessionCommand::ApproveHostKey(false));
        return;
    }
    focus_tab_for_session(app, session);
    let label = if mismatch {
        "Host key changed! This may indicate an attack — verify before trusting.".to_string()
    } else {
        "New host — verify the key fingerprint before trusting it.".to_string()
    };
    app.pending_prompt = Some(PendingPrompt::HostKey {
        session,
        ui: Prompt::HostKey {
            label,
            algorithm: key.algorithm,
            fingerprint: key.fingerprint,
            warning: mismatch,
        },
    });
}

/// Handles interaction with the pending credential/host-key modal.
fn update_prompt(app: &mut TermiteApp, message: PromptMessage) {
    match message {
        PromptMessage::InputChanged(value) => {
            if let Some(PendingPrompt::Credential {
                ui: Prompt::Credential { input, .. },
                ..
            }) = &mut app.pending_prompt
            {
                *input = value;
            }
        }
        PromptMessage::ToggleSave(value) => {
            if let Some(PendingPrompt::Credential {
                ui: Prompt::Credential { save, .. },
                ..
            }) = &mut app.pending_prompt
            {
                *save = value;
            }
        }
        PromptMessage::Submit => {
            if let Some(PendingPrompt::Credential {
                session,
                challenge,
                ui: Prompt::Credential { input, save, .. },
            }) = app.pending_prompt.take()
            {
                let secret = SecretString::from(input);
                if save {
                    if let Err(err) = save_credential(app, &challenge, &secret) {
                        tracing::error!(%err, "failed to save credential to keychain");
                    }
                }
                let response = match challenge {
                    AuthChallenge::Password { .. } => AuthResponse::Password(secret),
                    AuthChallenge::Passphrase { .. } => AuthResponse::Passphrase(secret),
                };
                send_to_session(app, session, SessionCommand::AuthResponse(response));
            }
        }
        PromptMessage::Cancel => {
            if let Some(PendingPrompt::Credential { session, .. }) = app.pending_prompt.take() {
                send_to_session(app, session, SessionCommand::Disconnect);
            }
        }
        PromptMessage::Approve => {
            if let Some(PendingPrompt::HostKey { session, .. }) = app.pending_prompt.take() {
                send_to_session(app, session, SessionCommand::ApproveHostKey(true));
            }
        }
        PromptMessage::Reject => {
            if let Some(PendingPrompt::HostKey { session, .. }) = app.pending_prompt.take() {
                send_to_session(app, session, SessionCommand::ApproveHostKey(false));
            }
        }
    }
}

/// Builds the `AuthMethod` a new host profile should be saved with from the
/// add-host form's selection. `key_path` is only consulted for `PublicKey`;
/// the caller (`update_sidebar`) already rejects an empty one before this
/// runs, so it isn't validated again here.
fn auth_method_from_form(kind: AuthKind, key_path: String) -> AuthMethod {
    match kind {
        AuthKind::Agent => AuthMethod::Agent,
        AuthKind::Password => AuthMethod::Password,
        AuthKind::PublicKey => AuthMethod::PublicKey {
            key_path: key_path.into(),
        },
    }
}

/// Splits the form's comma-separated tags field into a `Vec<String>`,
/// trimming whitespace and dropping empty entries (e.g. a trailing comma).
fn parse_tags(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_string)
        .collect()
}

/// Populates the add/edit-host form from an existing profile, in response
/// to `SidebarMessage::EditHost`. Clears any in-progress key generation
/// input — it belongs to whatever was being created/edited before, not this
/// newly-loaded profile.
fn load_profile_into_form(sidebar: &mut SidebarState, profile: &HostProfile) {
    sidebar.editing_id = Some(profile.id);
    sidebar.name_input = profile.name.clone();
    sidebar.address_input = profile.host.clone();
    sidebar.username_input = profile.username.clone();
    sidebar.tags_input = profile.tags.join(", ");
    sidebar.resolved_port = Some(profile.port);
    sidebar.resolved_hint = None;
    sidebar.keygen_comment_input.clear();
    sidebar.keygen_passphrase_input.clear();
    match &profile.auth {
        AuthMethod::Agent => {
            sidebar.auth_kind = AuthKind::Agent;
            sidebar.key_path_input.clear();
        }
        AuthMethod::Password => {
            sidebar.auth_kind = AuthKind::Password;
            sidebar.key_path_input.clear();
        }
        AuthMethod::PublicKey { key_path } => {
            sidebar.auth_kind = AuthKind::PublicKey;
            sidebar.key_path_input = key_path.display().to_string();
        }
    }
}

/// Builds a `HostProfile` for the host-import feature from one literal
/// `~/.ssh/config` alias and its resolved settings. `alias` becomes both the
/// profile's display name and, when the config has no `HostName`, its
/// connect address too — an alias with no `HostName` just *is* the host to
/// connect to, per OpenSSH semantics.
fn host_profile_from_config(alias: &str, resolved: &HostConfig) -> HostProfile {
    let host = resolved
        .host_name
        .clone()
        .unwrap_or_else(|| alias.to_string());
    let username = resolved.user.clone().unwrap_or_default();
    let mut profile = HostProfile::new(alias, host, username);
    profile.port = resolved.port.unwrap_or(22);
    profile.auth = match resolved.identity_files.first() {
        Some(key_path) => AuthMethod::PublicKey {
            key_path: key_path.clone(),
        },
        None => AuthMethod::Agent,
    };
    profile
}

/// Default location offered when the form's key-path field is empty and
/// "Generate key" is pressed: the app's own config directory rather than
/// the user's real `~/.ssh` (never touched unless the user explicitly types
/// a path there), with a numeric suffix appended if the default name is
/// already taken.
fn default_keygen_path() -> PathBuf {
    let base = dirs::config_dir()
        .map(|dir| dir.join("termite").join("keys"))
        .unwrap_or_else(|| PathBuf::from("."));
    first_available_key_path(&base)
}

/// The suffix-picking logic behind `default_keygen_path`, split out so it's
/// testable against a tempdir instead of the real config directory.
fn first_available_key_path(base: &Path) -> PathBuf {
    let mut candidate = base.join("id_ed25519");
    let mut suffix = 2;
    while candidate.exists() {
        candidate = base.join(format!("id_ed25519_{suffix}"));
        suffix += 1;
    }
    candidate
}

/// Generates a new ed25519 key pair and writes it to `path`. The caller must
/// have already checked `path` doesn't exist — this never overwrites.
/// `comment`/`passphrase` apply only if non-empty; a non-empty passphrase is
/// also saved to the keychain immediately, since the point of generating a
/// key from this form is for it to be usable right away — unlike an
/// existing key, nobody has had a chance to type this passphrase into the
/// credential prompt's own "save to keychain" toggle.
fn generate_and_save_key(
    path: &Path,
    comment: &str,
    passphrase: &str,
    credential_store: &Arc<dyn CredentialStore>,
) -> Result<String, TermiteError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut key = termite_crypto::key::generate_ed25519()?;
    if !comment.is_empty() {
        key.set_comment(comment);
    }
    let passphrase = (!passphrase.is_empty()).then(|| SecretString::from(passphrase.to_string()));
    termite_crypto::key::save_to_disk(&key, path, passphrase.as_ref())?;
    let fingerprint = termite_crypto::key::fingerprint(&key);
    if let Some(passphrase) = &passphrase {
        if let Err(err) = credential_store.set_passphrase(&fingerprint, passphrase) {
            tracing::warn!(%err, "failed to save newly-generated key's passphrase to keychain");
        }
    }
    Ok(fingerprint)
}

/// Applies a `~/.ssh/config` alias resolution to the add-host form. Only
/// fills in fields the user hasn't already typed a value into — a later
/// manual edit always wins over a config default — except the address
/// field itself and the port, where overwriting *is* the point of hitting
/// Enter on an alias. Called only when `resolved` is non-default; an
/// unmatched alias leaves the form untouched.
fn apply_resolved_config(sidebar: &mut SidebarState, resolved: &HostConfig) {
    if let Some(host_name) = &resolved.host_name {
        sidebar.address_input = host_name.clone();
    }
    if sidebar.username_input.is_empty() {
        if let Some(user) = &resolved.user {
            sidebar.username_input = user.clone();
        }
    }
    if resolved.port.is_some() {
        sidebar.resolved_port = resolved.port;
    }
    if sidebar.auth_kind == AuthKind::Agent {
        if let Some(key_path) = resolved.identity_files.first() {
            sidebar.auth_kind = AuthKind::PublicKey;
            sidebar.key_path_input = key_path.display().to_string();
        }
    }
    sidebar.resolved_hint = Some(resolution_hint(sidebar, resolved));
}

/// The text shown under the address field after a successful resolution.
fn resolution_hint(sidebar: &SidebarState, resolved: &HostConfig) -> String {
    let user_part = if sidebar.username_input.is_empty() {
        String::new()
    } else {
        format!("{}@", sidebar.username_input)
    };
    format!(
        "~/.ssh/config \u{2192} {user_part}{}:{}",
        sidebar.address_input,
        resolved.port.or(sidebar.resolved_port).unwrap_or(22)
    )
}

fn update_sidebar(app: &mut TermiteApp, message: SidebarMessage) -> Task<Message> {
    match message {
        SidebarMessage::NameInputChanged(value) => {
            app.sidebar.name_input = value;
        }
        SidebarMessage::AddressInputChanged(value) => {
            app.sidebar.address_input = value;
            // A prior resolution no longer describes the (now-different)
            // address; ResolveAlias will re-derive it if the new value
            // also happens to be an alias.
            app.sidebar.resolved_port = None;
            app.sidebar.resolved_hint = None;
        }
        SidebarMessage::UsernameInputChanged(value) => {
            app.sidebar.username_input = value;
        }
        SidebarMessage::AuthKindSelected(kind) => {
            app.sidebar.auth_kind = kind;
        }
        SidebarMessage::KeyPathInputChanged(value) => {
            app.sidebar.key_path_input = value;
        }
        SidebarMessage::ResolveAlias => {
            let resolved = app.ssh_config.query(&app.sidebar.address_input);
            if resolved != HostConfig::default() {
                apply_resolved_config(&mut app.sidebar, &resolved);
            }
        }
        SidebarMessage::TagsInputChanged(value) => {
            app.sidebar.tags_input = value;
        }
        SidebarMessage::SearchInputChanged(value) => {
            app.sidebar.search_input = value;
        }
        SidebarMessage::KeygenCommentChanged(value) => {
            app.sidebar.keygen_comment_input = value;
        }
        SidebarMessage::KeygenPassphraseChanged(value) => {
            app.sidebar.keygen_passphrase_input = value;
        }
        SidebarMessage::EditHost(id) => {
            if let Some(profile) = app.hosts.iter().find(|host| host.id == id).cloned() {
                load_profile_into_form(&mut app.sidebar, &profile);
            }
        }
        SidebarMessage::CancelEdit => {
            app.sidebar = SidebarState::default();
        }
        SidebarMessage::SaveHost => {
            // A public-key profile with no path would fail to connect with
            // no way to fix it short of deleting and re-adding the host, so
            // it's rejected here rather than saved.
            let key_path_ok = app.sidebar.auth_kind != AuthKind::PublicKey
                || !app.sidebar.key_path_input.is_empty();
            if app.sidebar.name_input.is_empty()
                || app.sidebar.address_input.is_empty()
                || !key_path_ok
            {
                return Task::none();
            }

            let tags = parse_tags(&app.sidebar.tags_input);
            let auth = auth_method_from_form(
                app.sidebar.auth_kind,
                std::mem::take(&mut app.sidebar.key_path_input),
            );
            let port = app.sidebar.resolved_port.take().unwrap_or(22);

            let profile = match app.sidebar.editing_id {
                // Preserve id/favourite/last_connected — the form doesn't
                // surface any of the three, so rebuilding from scratch
                // would silently unstar or forget the recency of an edited
                // host.
                Some(id) => {
                    let existing = app.hosts.iter().find(|host| host.id == id);
                    HostProfile {
                        id,
                        name: std::mem::take(&mut app.sidebar.name_input),
                        host: std::mem::take(&mut app.sidebar.address_input),
                        port,
                        username: std::mem::take(&mut app.sidebar.username_input),
                        auth,
                        tags,
                        favourite: existing.is_some_and(|host| host.favourite),
                        last_connected: existing.and_then(|host| host.last_connected),
                    }
                }
                None => {
                    let mut profile = HostProfile::new(
                        std::mem::take(&mut app.sidebar.name_input),
                        std::mem::take(&mut app.sidebar.address_input),
                        std::mem::take(&mut app.sidebar.username_input),
                    );
                    profile.port = port;
                    profile.auth = auth;
                    profile.tags = tags;
                    profile
                }
            };

            app.sidebar = SidebarState::default();
            let store = Arc::clone(&app.host_store);
            return Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        if let Err(err) = store.save(profile) {
                            tracing::error!(%err, "failed to save host profile");
                        }
                        store.list().unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default()
                },
                Message::HostsLoaded,
            );
        }
        SidebarMessage::DeleteHost(id) => {
            if let Some(profile) = app.hosts.iter().find(|host| host.id == id) {
                forget_credential(&app.credential_store, profile);
            }
            let store = Arc::clone(&app.host_store);
            return Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        if let Err(err) = store.delete(id) {
                            tracing::error!(%err, "failed to delete host profile");
                        }
                        store.list().unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default()
                },
                Message::HostsLoaded,
            );
        }
        SidebarMessage::ToggleFavourite(id) => {
            if let Some(mut profile) = app.hosts.iter().find(|host| host.id == id).cloned() {
                profile.favourite = !profile.favourite;
                let store = Arc::clone(&app.host_store);
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            if let Err(err) = store.save(profile) {
                                tracing::error!(%err, "failed to persist favourite toggle");
                            }
                            store.list().unwrap_or_default()
                        })
                        .await
                        .unwrap_or_default()
                    },
                    Message::HostsLoaded,
                );
            }
        }
        SidebarMessage::SelectHost(id) => {
            if let Some(mut profile) = app.hosts.iter().find(|host| host.id == id).cloned() {
                let (rows, cols) = app.grid_size;
                let tab = Tab::new(
                    profile.name.clone(),
                    TabKind::Ssh {
                        session_id: None,
                        profile: profile.clone(),
                        reconnect_attempt: 0,
                    },
                    ConnectionStatus::Connecting,
                    rows,
                    cols,
                );
                let tab_id = tab.id;
                app.tabs.push(tab);
                app.active_tab = Some(tab_id);
                match &app.ssh_worker {
                    Some(sender) => {
                        let mut sender = sender.clone();
                        let _ = sender.try_send(SshWorkerInput::Connect(tab_id, profile.clone()));
                    }
                    None => tracing::warn!("ssh worker not ready yet; connect request dropped"),
                }
                profile.last_connected = Some(unix_now());
                let store = Arc::clone(&app.host_store);
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            if let Err(err) = store.save(profile) {
                                tracing::error!(%err, "failed to persist last-connected timestamp");
                            }
                            store.list().unwrap_or_default()
                        })
                        .await
                        .unwrap_or_default()
                    },
                    Message::HostsLoaded,
                );
            }
        }
        SidebarMessage::ImportFromSshConfig => {
            let existing_names: HashSet<String> = app
                .hosts
                .iter()
                .map(|host| host.name.to_ascii_lowercase())
                .collect();
            let new_profiles: Vec<HostProfile> = app
                .ssh_config
                .host_aliases()
                .into_iter()
                .filter(|alias| !existing_names.contains(alias))
                .map(|alias| {
                    let resolved = app.ssh_config.query(&alias);
                    host_profile_from_config(&alias, &resolved)
                })
                .collect();

            if new_profiles.is_empty() {
                advance_active(
                    app,
                    b"\r\n*** no new hosts to import from ~/.ssh/config ***\r\n",
                );
                return Task::none();
            }

            let count = new_profiles.len();
            advance_active(
                app,
                format!("\r\n*** importing {count} host(s) from ~/.ssh/config ***\r\n").as_bytes(),
            );
            let store = Arc::clone(&app.host_store);
            return Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        for profile in new_profiles {
                            if let Err(err) = store.save(profile) {
                                tracing::error!(%err, "failed to save imported host profile");
                            }
                        }
                        store.list().unwrap_or_default()
                    })
                    .await
                    .unwrap_or_default()
                },
                Message::HostsLoaded,
            );
        }
        SidebarMessage::GenerateKey => {
            let path = if app.sidebar.key_path_input.is_empty() {
                default_keygen_path()
            } else {
                PathBuf::from(&app.sidebar.key_path_input)
            };
            if path.exists() {
                advance_active(
                    app,
                    format!(
                        "\r\n*** refusing to generate key: {} already exists ***\r\n",
                        path.display()
                    )
                    .as_bytes(),
                );
                return Task::none();
            }
            let comment = std::mem::take(&mut app.sidebar.keygen_comment_input);
            let passphrase = std::mem::take(&mut app.sidebar.keygen_passphrase_input);
            match generate_and_save_key(&path, &comment, &passphrase, &app.credential_store) {
                Ok(fingerprint) => {
                    app.sidebar.key_path_input = path.display().to_string();
                    app.sidebar.auth_kind = AuthKind::PublicKey;
                    advance_active(
                        app,
                        format!(
                            "\r\n*** generated ed25519 key {} ({fingerprint}) ***\r\n",
                            path.display()
                        )
                        .as_bytes(),
                    );
                }
                Err(err) => {
                    tracing::error!(%err, "key generation failed");
                    advance_active(app, b"\r\n*** key generation failed; see logs ***\r\n");
                }
            }
        }
    }
    Task::none()
}

fn view(app: &TermiteApp) -> Element<'_, Message> {
    let sidebar = sidebar::view(&app.hosts, &app.sidebar).map(Message::Sidebar);

    let summaries = tab_summaries(app);
    let tab_bar = tabbar::view(&summaries, app.active_tab).map(Message::TabBar);

    let (rows, selection) = app
        .active_tab()
        .map(|tab| (grid_char_rows(&tab.grid), tab.selection))
        .unwrap_or_default();
    let bell_flash = app.active_tab().is_some_and(|tab| tab.bell_flash);
    let spans = terminal_spans(&rows, selection.as_ref());
    let terminal: Element<'_, Message> = Rich::with_spans(spans)
        .font(Font::MONOSPACE)
        .size(14)
        .width(Length::Fill)
        .into();
    let terminal = container(terminal).style(move |_theme| {
        if bell_flash {
            container::Style {
                border: iced::Border {
                    color: termite_ui::theme::colours::ACCENT,
                    width: 2.0,
                    radius: 0.0.into(),
                },
                ..container::Style::default()
            }
        } else {
            container::Style::default()
        }
    });
    let terminal = mouse_area(terminal)
        .on_press(Message::MousePress(iced::mouse::Button::Left))
        .on_release(Message::MouseRelease(iced::mouse::Button::Left))
        .on_right_press(Message::MousePress(iced::mouse::Button::Right))
        .on_right_release(Message::MouseRelease(iced::mouse::Button::Right))
        .on_middle_press(Message::MousePress(iced::mouse::Button::Middle))
        .on_middle_release(Message::MouseRelease(iced::mouse::Button::Middle))
        .on_move(Message::MouseMoved)
        .on_scroll(Message::MouseScrolled);

    let pane = column![tab_bar, terminal].width(Length::Fill);

    let content: Element<'_, Message> = row![sidebar, pane].into();

    match &app.pending_prompt {
        Some(pending) => {
            let modal = prompt::view(pending.ui()).map(Message::Prompt);
            Stack::new().push(content).push(modal).into()
        }
        None => content,
    }
}

fn subscription(_app: &TermiteApp) -> Subscription<Message> {
    Subscription::batch([
        iced::time::every(POLL_INTERVAL).map(|_| Message::PollOutput),
        iced::keyboard::on_key_press(|key, modifiers| Some(Message::KeyPressed { key, modifiers })),
        iced::window::resize_events().map(|(_id, size)| Message::WindowResized(size)),
        Subscription::run(ssh_worker),
    ])
}

/// Converts a key press into the byte sequence to send to the shell.
fn key_to_bytes(key: &Key, modifiers: Modifiers) -> Option<Vec<u8>> {
    if modifiers.control() {
        if let Key::Character(c) = key {
            let c = c.chars().next()?;
            if c.is_ascii_alphabetic() {
                let byte = (c.to_ascii_lowercase() as u8) - b'a' + 1;
                return Some(vec![byte]);
            }
        }
    }

    match key {
        Key::Named(Named::Enter) => Some(vec![b'\r']),
        Key::Named(Named::Backspace) => Some(vec![0x7f]),
        Key::Named(Named::Tab) => Some(vec![b'\t']),
        Key::Named(Named::Escape) => Some(vec![0x1b]),
        Key::Named(Named::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(Named::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(Named::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(Named::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(Named::Space) => Some(vec![b' ']),
        Key::Character(c) => Some(c.as_bytes().to_vec()),
        _ => None,
    }
}

// ── Logging setup ─────────────────────────────────────────────────────────────

fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("termite=info")),
        )
        .init();
}

// ── Tests ─────────────────────────────────────────────────────────────────────
//
// No live SSH server or GUI is available in this environment (see
// `HANDOFF.md`'s "Verification limits"/"Isolated GUI testing" notes), so
// these exercise the prompt state machine directly — `handle_session_event`,
// `update_prompt`, and `auth_method_from_form` — against a real `TermiteApp`
// but with a fake `ssh_worker` channel standing in for the subscription, so
// exactly what `SshWorkerInput` a handler sends can be asserted without a
// real `SshSession` or Iced runtime.
#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::path::PathBuf;
    use termite_core::HostId;
    use termite_ssh::DisconnectReason;
    use termite_storage::MemoryStore;

    fn test_app() -> TermiteApp {
        let mut app = TermiteApp::new().expect("failed to spawn local shell pty for test");
        // Real `KeyringStore` would work here too (CI runs under a real
        // Secret Service/Keychain/Credential Manager — see `ci.yml`), but a
        // `MemoryStore` keeps these tests from writing test credentials into
        // whatever real keychain happens to be running wherever they're run.
        app.credential_store = Arc::new(MemoryStore::new());
        app
    }

    fn wire_fake_worker(app: &mut TermiteApp) -> bridge::Receiver<SshWorkerInput> {
        let (tx, rx) = bridge::channel(8);
        app.ssh_worker = Some(tx);
        rx
    }

    fn active_grid_rows(app: &TermiteApp) -> Vec<String> {
        app.active_tab().expect("an active tab").grid.visible_rows()
    }

    fn test_host_key() -> HostKey {
        HostKey {
            algorithm: "ssh-ed25519".to_string(),
            fingerprint: "SHA256:xyz".to_string(),
        }
    }

    fn test_password_challenge() -> AuthChallenge {
        AuthChallenge::Password {
            host: "example.com".to_string(),
            username: "alice".to_string(),
        }
    }

    #[test]
    fn auth_required_password_opens_credential_prompt() {
        let mut app = test_app();
        let id = SessionId::new();

        let _ = handle_session_event(
            &mut app,
            id,
            SessionEvent::AuthRequired(test_password_challenge()),
        );

        match &app.pending_prompt {
            Some(PendingPrompt::Credential {
                session,
                challenge,
                ui: Prompt::Credential { label, input, save },
            }) => {
                assert_eq!(*session, id);
                assert_eq!(*challenge, test_password_challenge());
                assert!(label.contains("Password"));
                assert!(input.is_empty());
                assert!(!save);
            }
            other => panic!("expected a credential prompt, got {other:?}"),
        }
    }

    #[test]
    fn auth_required_passphrase_label_includes_fingerprint() {
        let mut app = test_app();
        let id = SessionId::new();

        let _ = handle_session_event(
            &mut app,
            id,
            SessionEvent::AuthRequired(AuthChallenge::Passphrase {
                fingerprint: "SHA256:abc".to_string(),
            }),
        );

        match &app.pending_prompt {
            Some(PendingPrompt::Credential {
                ui: Prompt::Credential { label, .. },
                ..
            }) => assert!(label.contains("SHA256:abc")),
            other => panic!("expected a credential prompt, got {other:?}"),
        }
    }

    #[test]
    fn host_key_unknown_opens_prompt_without_warning() {
        let mut app = test_app();
        let id = SessionId::new();
        let key = test_host_key();

        let _ = handle_session_event(&mut app, id, SessionEvent::HostKeyUnknown(key.clone()));

        match &app.pending_prompt {
            Some(PendingPrompt::HostKey {
                session,
                ui:
                    Prompt::HostKey {
                        algorithm,
                        fingerprint,
                        warning,
                        ..
                    },
            }) => {
                assert_eq!(*session, id);
                assert_eq!(algorithm, &key.algorithm);
                assert_eq!(fingerprint, &key.fingerprint);
                assert!(!warning);
            }
            other => panic!("expected a host-key prompt, got {other:?}"),
        }
    }

    #[test]
    fn host_key_mismatch_opens_prompt_with_warning() {
        let mut app = test_app();
        let id = SessionId::new();

        let _ = handle_session_event(&mut app, id, SessionEvent::HostKeyMismatch(test_host_key()));

        match &app.pending_prompt {
            Some(PendingPrompt::HostKey {
                ui: Prompt::HostKey { warning, .. },
                ..
            }) => assert!(*warning),
            other => panic!("expected a host-key prompt, got {other:?}"),
        }
    }

    #[test]
    fn second_auth_required_fails_closed_and_leaves_first_prompt_intact() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let first = SessionId::new();
        let second = SessionId::new();

        let _ = handle_session_event(
            &mut app,
            first,
            SessionEvent::AuthRequired(test_password_challenge()),
        );
        let _ = handle_session_event(
            &mut app,
            second,
            SessionEvent::AuthRequired(test_password_challenge()),
        );

        match &app.pending_prompt {
            Some(PendingPrompt::Credential { session, .. }) => assert_eq!(*session, first),
            other => panic!("expected the first prompt to survive, got {other:?}"),
        }
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(id, SessionCommand::Disconnect)) => {
                assert_eq!(id, second)
            }
            other => panic!("expected a Disconnect for the second session, got {other:?}"),
        }
    }

    #[test]
    fn second_host_key_event_fails_closed_and_rejects() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let first = SessionId::new();
        let second = SessionId::new();

        let _ = handle_session_event(
            &mut app,
            first,
            SessionEvent::HostKeyUnknown(test_host_key()),
        );
        let _ = handle_session_event(
            &mut app,
            second,
            SessionEvent::HostKeyUnknown(test_host_key()),
        );

        match &app.pending_prompt {
            Some(PendingPrompt::HostKey { session, .. }) => assert_eq!(*session, first),
            other => panic!("expected the first prompt to survive, got {other:?}"),
        }
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(id, SessionCommand::ApproveHostKey(false))) => {
                assert_eq!(id, second)
            }
            other => panic!("expected a rejection for the second session, got {other:?}"),
        }
    }

    #[test]
    fn submit_sends_password_auth_response_and_clears_prompt() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let id = SessionId::new();

        let _ = handle_session_event(
            &mut app,
            id,
            SessionEvent::AuthRequired(test_password_challenge()),
        );
        update_prompt(&mut app, PromptMessage::InputChanged("hunter2".to_string()));
        update_prompt(&mut app, PromptMessage::Submit);

        assert!(app.pending_prompt.is_none());
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(
                sent_id,
                SessionCommand::AuthResponse(AuthResponse::Password(secret)),
            )) => {
                assert_eq!(sent_id, id);
                assert_eq!(secret.expose_secret(), "hunter2");
            }
            other => panic!("expected a password AuthResponse, got {other:?}"),
        }
    }

    #[test]
    fn submit_with_save_checked_persists_password_to_credential_store() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let id = SessionId::new();

        let _ = handle_session_event(
            &mut app,
            id,
            SessionEvent::AuthRequired(test_password_challenge()),
        );
        update_prompt(&mut app, PromptMessage::InputChanged("hunter2".to_string()));
        update_prompt(&mut app, PromptMessage::ToggleSave(true));
        update_prompt(&mut app, PromptMessage::Submit);

        // Submit still answers the challenge as normal...
        assert!(matches!(
            rx.try_recv(),
            Ok(SshWorkerInput::Send(
                _,
                SessionCommand::AuthResponse(AuthResponse::Password(_))
            ))
        ));

        // ...and the credential is now retrievable from the store directly.
        let saved = app
            .credential_store
            .get_password("example.com", "alice")
            .unwrap()
            .expect("password should have been saved");
        assert_eq!(saved.expose_secret(), "hunter2");
    }

    #[test]
    fn submit_without_save_does_not_persist_to_credential_store() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let id = SessionId::new();

        let _ = handle_session_event(
            &mut app,
            id,
            SessionEvent::AuthRequired(test_password_challenge()),
        );
        update_prompt(&mut app, PromptMessage::InputChanged("hunter2".to_string()));
        update_prompt(&mut app, PromptMessage::Submit);
        let _ = rx.try_recv();

        assert!(app
            .credential_store
            .get_password("example.com", "alice")
            .unwrap()
            .is_none());
    }

    #[test]
    fn auth_required_auto_answers_from_a_saved_password_without_prompting() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let id = SessionId::new();
        app.credential_store
            .set_password("example.com", "alice", &SecretString::from("hunter2"))
            .unwrap();

        let _ = handle_session_event(
            &mut app,
            id,
            SessionEvent::AuthRequired(test_password_challenge()),
        );

        assert!(app.pending_prompt.is_none());
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(
                sent_id,
                SessionCommand::AuthResponse(AuthResponse::Password(secret)),
            )) => {
                assert_eq!(sent_id, id);
                assert_eq!(secret.expose_secret(), "hunter2");
            }
            other => panic!("expected an auto-answered password AuthResponse, got {other:?}"),
        }
    }

    #[test]
    fn submit_passphrase_sends_passphrase_auth_response() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let id = SessionId::new();

        let _ = handle_session_event(
            &mut app,
            id,
            SessionEvent::AuthRequired(AuthChallenge::Passphrase {
                fingerprint: "SHA256:abc".to_string(),
            }),
        );
        update_prompt(
            &mut app,
            PromptMessage::InputChanged("swordfish".to_string()),
        );
        update_prompt(&mut app, PromptMessage::Submit);

        match rx.try_recv() {
            Ok(SshWorkerInput::Send(
                _,
                SessionCommand::AuthResponse(AuthResponse::Passphrase(secret)),
            )) => {
                assert_eq!(secret.expose_secret(), "swordfish");
            }
            other => panic!("expected a passphrase AuthResponse, got {other:?}"),
        }
    }

    #[test]
    fn cancel_disconnects_and_clears_prompt() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let id = SessionId::new();

        let _ = handle_session_event(
            &mut app,
            id,
            SessionEvent::AuthRequired(test_password_challenge()),
        );
        update_prompt(&mut app, PromptMessage::Cancel);

        assert!(app.pending_prompt.is_none());
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(sent_id, SessionCommand::Disconnect)) => {
                assert_eq!(sent_id, id)
            }
            other => panic!("expected a Disconnect, got {other:?}"),
        }
    }

    #[test]
    fn approve_sends_approve_host_key_true() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let id = SessionId::new();

        let _ = handle_session_event(&mut app, id, SessionEvent::HostKeyUnknown(test_host_key()));
        update_prompt(&mut app, PromptMessage::Approve);

        assert!(app.pending_prompt.is_none());
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(sent_id, SessionCommand::ApproveHostKey(true))) => {
                assert_eq!(sent_id, id)
            }
            other => panic!("expected an ApproveHostKey(true), got {other:?}"),
        }
    }

    #[test]
    fn reject_sends_approve_host_key_false() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let id = SessionId::new();

        let _ = handle_session_event(&mut app, id, SessionEvent::HostKeyMismatch(test_host_key()));
        update_prompt(&mut app, PromptMessage::Reject);

        assert!(app.pending_prompt.is_none());
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(sent_id, SessionCommand::ApproveHostKey(false))) => {
                assert_eq!(sent_id, id)
            }
            other => panic!("expected an ApproveHostKey(false), got {other:?}"),
        }
    }

    #[test]
    fn disconnect_clears_prompt_for_its_own_session() {
        let mut app = test_app();
        let id = SessionId::new();
        let _ = handle_session_event(
            &mut app,
            id,
            SessionEvent::AuthRequired(test_password_challenge()),
        );
        assert!(app.pending_prompt.is_some());

        let _ = handle_session_event(
            &mut app,
            id,
            SessionEvent::Disconnected {
                reason: DisconnectReason::Remote,
            },
        );

        assert!(app.pending_prompt.is_none());
    }

    #[test]
    fn disconnect_of_a_different_session_leaves_prompt_intact() {
        let mut app = test_app();
        let prompting = SessionId::new();
        let other = SessionId::new();
        let _ = handle_session_event(
            &mut app,
            prompting,
            SessionEvent::AuthRequired(test_password_challenge()),
        );

        let _ = handle_session_event(
            &mut app,
            other,
            SessionEvent::Disconnected {
                reason: DisconnectReason::Remote,
            },
        );

        assert!(app.pending_prompt.is_some());
    }

    #[test]
    fn auth_method_from_form_maps_each_kind() {
        assert_eq!(
            auth_method_from_form(AuthKind::Agent, String::new()),
            AuthMethod::Agent
        );
        assert_eq!(
            auth_method_from_form(AuthKind::Password, String::new()),
            AuthMethod::Password
        );
        assert_eq!(
            auth_method_from_form(
                AuthKind::PublicKey,
                "/home/user/.ssh/id_ed25519".to_string()
            ),
            AuthMethod::PublicKey {
                key_path: PathBuf::from("/home/user/.ssh/id_ed25519"),
            },
        );
    }

    #[test]
    fn apply_resolved_config_fills_empty_fields_and_sets_hint() {
        let mut sidebar = SidebarState {
            address_input: "work".to_string(),
            ..SidebarState::default()
        };
        let resolved = HostConfig {
            host_name: Some("gitlab.internal.example.com".to_string()),
            user: Some("deploy".to_string()),
            port: Some(2222),
            ..HostConfig::default()
        };

        apply_resolved_config(&mut sidebar, &resolved);

        assert_eq!(sidebar.address_input, "gitlab.internal.example.com");
        assert_eq!(sidebar.username_input, "deploy");
        assert_eq!(sidebar.resolved_port, Some(2222));
        assert_eq!(sidebar.auth_kind, AuthKind::Agent);
        assert_eq!(
            sidebar.resolved_hint.as_deref(),
            Some("~/.ssh/config \u{2192} deploy@gitlab.internal.example.com:2222")
        );
    }

    #[test]
    fn apply_resolved_config_does_not_override_a_typed_username() {
        let mut sidebar = SidebarState {
            address_input: "work".to_string(),
            username_input: "alice".to_string(),
            ..SidebarState::default()
        };
        let resolved = HostConfig {
            user: Some("bob".to_string()),
            ..HostConfig::default()
        };

        apply_resolved_config(&mut sidebar, &resolved);

        assert_eq!(sidebar.username_input, "alice");
    }

    #[test]
    fn apply_resolved_config_switches_to_public_key_auth_when_a_key_is_resolved() {
        let mut sidebar = SidebarState::default();
        let resolved = HostConfig {
            identity_files: vec![PathBuf::from("/keys/id_ed25519")],
            ..HostConfig::default()
        };

        apply_resolved_config(&mut sidebar, &resolved);

        assert_eq!(sidebar.auth_kind, AuthKind::PublicKey);
        assert_eq!(sidebar.key_path_input, "/keys/id_ed25519");
    }

    #[test]
    fn apply_resolved_config_does_not_override_an_explicit_auth_kind() {
        let mut sidebar = SidebarState {
            auth_kind: AuthKind::Password,
            ..SidebarState::default()
        };
        let resolved = HostConfig {
            identity_files: vec![PathBuf::from("/keys/id_ed25519")],
            ..HostConfig::default()
        };

        apply_resolved_config(&mut sidebar, &resolved);

        assert_eq!(sidebar.auth_kind, AuthKind::Password);
        assert!(sidebar.key_path_input.is_empty());
    }

    #[test]
    fn resolve_alias_message_matches_a_configured_host() {
        let mut app = test_app();
        app.ssh_config =
            SshConfig::parse("Host work\n\tHostName gitlab.internal.example.com\n\tPort 2222\n")
                .expect("valid ssh_config text");
        app.sidebar.address_input = "work".to_string();

        let _ = update_sidebar(&mut app, SidebarMessage::ResolveAlias);

        assert_eq!(app.sidebar.address_input, "gitlab.internal.example.com");
        assert_eq!(app.sidebar.resolved_port, Some(2222));
        assert!(app.sidebar.resolved_hint.is_some());
    }

    #[test]
    fn resolve_alias_message_is_a_no_op_for_an_unmatched_address() {
        let mut app = test_app();
        app.sidebar.address_input = "no-such-alias.example.com".to_string();

        let _ = update_sidebar(&mut app, SidebarMessage::ResolveAlias);

        assert_eq!(app.sidebar.address_input, "no-such-alias.example.com");
        assert_eq!(app.sidebar.resolved_port, None);
        assert_eq!(app.sidebar.resolved_hint, None);
    }

    #[test]
    fn changing_the_address_clears_a_stale_resolution() {
        let mut app = test_app();
        app.sidebar.resolved_port = Some(2222);
        app.sidebar.resolved_hint = Some("stale".to_string());

        let _ = update_sidebar(
            &mut app,
            SidebarMessage::AddressInputChanged("something-else".to_string()),
        );

        assert_eq!(app.sidebar.resolved_port, None);
        assert_eq!(app.sidebar.resolved_hint, None);
    }

    #[test]
    fn save_host_resets_the_whole_form() {
        let mut app = test_app();
        app.sidebar.name_input = "Work".to_string();
        app.sidebar.address_input = "gitlab.internal.example.com".to_string();
        app.sidebar.username_input = "deploy".to_string();
        app.sidebar.resolved_port = Some(2222);
        app.sidebar.resolved_hint = Some("~/.ssh/config \u{2192} deploy@host:2222".to_string());

        let _ = update_sidebar(&mut app, SidebarMessage::SaveHost);

        assert_eq!(app.sidebar.resolved_port, None);
        assert_eq!(app.sidebar.resolved_hint, None);
        assert!(app.sidebar.name_input.is_empty());
    }

    #[test]
    fn forget_credential_deletes_a_saved_password() {
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
        let profile = HostProfile {
            auth: AuthMethod::Password,
            ..HostProfile::new("Work", "example.com", "alice")
        };
        store
            .set_password(
                &profile.host,
                &profile.username,
                &SecretString::from("hunter2".to_string()),
            )
            .unwrap();

        forget_credential(&store, &profile);

        assert!(store
            .get_password(&profile.host, &profile.username)
            .unwrap()
            .is_none());
    }

    #[test]
    fn forget_credential_deletes_a_saved_passphrase_by_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("id_ed25519");
        let key = termite_crypto::key::generate_ed25519().unwrap();
        let passphrase = SecretString::from("correct horse battery staple".to_string());
        termite_crypto::key::save_to_disk(&key, &key_path, Some(&passphrase)).unwrap();
        let fingerprint = termite_crypto::key::fingerprint(&key);

        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
        store.set_passphrase(&fingerprint, &passphrase).unwrap();

        let profile = HostProfile {
            auth: AuthMethod::PublicKey { key_path },
            ..HostProfile::new("Work", "example.com", "alice")
        };
        forget_credential(&store, &profile);

        assert!(store.get_passphrase(&fingerprint).unwrap().is_none());
    }

    #[test]
    fn forget_credential_is_a_no_op_for_agent_auth() {
        // Agent auth never has a keychain entry to begin with; this just
        // confirms the match arm doesn't panic or touch the store.
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());
        let profile = HostProfile::new("Work", "example.com", "alice");

        forget_credential(&store, &profile);
    }

    #[test]
    fn delete_host_forgets_its_saved_password() {
        let mut app = test_app();
        let profile = HostProfile {
            auth: AuthMethod::Password,
            ..HostProfile::new("Work", "example.com", "alice")
        };
        app.credential_store
            .set_password(
                &profile.host,
                &profile.username,
                &SecretString::from("hunter2".to_string()),
            )
            .unwrap();
        let id = profile.id;
        app.hosts.push(profile);

        let _ = update_sidebar(&mut app, SidebarMessage::DeleteHost(id));

        assert!(app
            .credential_store
            .get_password("example.com", "alice")
            .unwrap()
            .is_none());
    }

    // ── M4 completion: favourites, recent connections, search, tags,
    // profile editor, ~/.ssh/config import, key generation ─────────────────
    //
    // `SaveHost`/`ToggleFavourite`/`SelectHost`/`ImportFromSshConfig` all
    // persist via `Task::perform(spawn_blocking(...), Message::HostsLoaded)`
    // — same as the pre-existing `DeleteHost` handler above — and that Task
    // is never polled by a plain #[test] fn (there's no Iced runtime or
    // `#[tokio::test]` driving it here). So, consistent with
    // `delete_host_forgets_its_saved_password` above, these are tested at
    // the level of what happens *before* the Task is returned (form resets,
    // ssh_worker sends) and the pure helper functions are tested directly
    // for the persisted-value logic. `GenerateKey` is the exception: it has
    // no Task, so it's fully testable end-to-end at the message level.

    #[test]
    fn sort_hosts_orders_favourites_first_then_recency_then_name() {
        let mut old_recent = HostProfile::new("Bravo", "b.example.com", "alice");
        old_recent.last_connected = Some(100);
        let mut new_recent = HostProfile::new("Alpha", "a.example.com", "alice");
        new_recent.last_connected = Some(200);
        let never_connected = HostProfile::new("Zulu", "z.example.com", "alice");
        let mut favourite = HostProfile::new("Charlie", "c.example.com", "alice");
        favourite.favourite = true;

        let mut hosts = vec![
            old_recent.clone(),
            never_connected.clone(),
            new_recent.clone(),
            favourite.clone(),
        ];
        sort_hosts(&mut hosts);

        assert_eq!(
            hosts.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            vec!["Charlie", "Alpha", "Bravo", "Zulu"]
        );
    }

    #[test]
    fn parse_tags_trims_and_drops_empty_entries() {
        assert_eq!(
            parse_tags(" prod, db , , staging"),
            vec!["prod".to_string(), "db".to_string(), "staging".to_string()]
        );
        assert_eq!(parse_tags(""), Vec::<String>::new());
    }

    #[test]
    fn load_profile_into_form_populates_password_auth_fields() {
        let mut sidebar = SidebarState::default();
        let profile = HostProfile {
            port: 2200,
            tags: vec!["prod".to_string(), "db".to_string()],
            auth: AuthMethod::Password,
            ..HostProfile::new("Work", "example.com", "alice")
        };

        load_profile_into_form(&mut sidebar, &profile);

        assert_eq!(sidebar.editing_id, Some(profile.id));
        assert_eq!(sidebar.name_input, "Work");
        assert_eq!(sidebar.address_input, "example.com");
        assert_eq!(sidebar.username_input, "alice");
        assert_eq!(sidebar.tags_input, "prod, db");
        assert_eq!(sidebar.resolved_port, Some(2200));
        assert_eq!(sidebar.auth_kind, AuthKind::Password);
        assert!(sidebar.key_path_input.is_empty());
    }

    #[test]
    fn load_profile_into_form_switches_to_public_key_and_fills_path() {
        let mut sidebar = SidebarState::default();
        let profile = HostProfile {
            auth: AuthMethod::PublicKey {
                key_path: PathBuf::from("/keys/id_ed25519"),
            },
            ..HostProfile::new("Work", "example.com", "alice")
        };

        load_profile_into_form(&mut sidebar, &profile);

        assert_eq!(sidebar.auth_kind, AuthKind::PublicKey);
        assert_eq!(sidebar.key_path_input, "/keys/id_ed25519");
    }

    #[test]
    fn host_profile_from_config_uses_alias_as_host_when_no_hostname() {
        let resolved = HostConfig {
            user: Some("deploy".to_string()),
            port: Some(2222),
            ..HostConfig::default()
        };

        let profile = host_profile_from_config("work", &resolved);

        assert_eq!(profile.name, "work");
        assert_eq!(profile.host, "work");
        assert_eq!(profile.username, "deploy");
        assert_eq!(profile.port, 2222);
        assert_eq!(profile.auth, AuthMethod::Agent);
    }

    #[test]
    fn host_profile_from_config_prefers_identity_file_over_agent() {
        let resolved = HostConfig {
            host_name: Some("gitlab.internal.example.com".to_string()),
            identity_files: vec![PathBuf::from("/keys/id_ed25519")],
            ..HostConfig::default()
        };

        let profile = host_profile_from_config("work", &resolved);

        assert_eq!(profile.host, "gitlab.internal.example.com");
        assert_eq!(
            profile.auth,
            AuthMethod::PublicKey {
                key_path: PathBuf::from("/keys/id_ed25519")
            }
        );
    }

    #[test]
    fn first_available_key_path_appends_suffix_when_taken() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            first_available_key_path(dir.path()),
            dir.path().join("id_ed25519")
        );

        std::fs::write(dir.path().join("id_ed25519"), b"taken").unwrap();
        assert_eq!(
            first_available_key_path(dir.path()),
            dir.path().join("id_ed25519_2")
        );

        std::fs::write(dir.path().join("id_ed25519_2"), b"also taken").unwrap();
        assert_eq!(
            first_available_key_path(dir.path()),
            dir.path().join("id_ed25519_3")
        );
    }

    #[test]
    fn generate_and_save_key_without_passphrase_does_not_touch_credential_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());

        let fingerprint = generate_and_save_key(&path, "", "", &store).unwrap();

        assert!(path.exists());
        assert!(store.get_passphrase(&fingerprint).unwrap().is_none());
    }

    #[test]
    fn generate_and_save_key_with_passphrase_saves_it_by_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        let store: Arc<dyn CredentialStore> = Arc::new(MemoryStore::new());

        let fingerprint = generate_and_save_key(&path, "work laptop", "hunter2", &store).unwrap();

        let saved = store.get_passphrase(&fingerprint).unwrap().unwrap();
        assert_eq!(saved.expose_secret(), "hunter2");

        // The comment made it onto the key itself, and the key is
        // encrypted (a passphrase was given).
        let loaded = termite_crypto::key::load(&path).unwrap();
        assert!(loaded.is_encrypted());
    }

    #[test]
    fn edit_host_message_loads_profile_into_form() {
        let mut app = test_app();
        let profile = HostProfile::new("Work", "example.com", "alice");
        let id = profile.id;
        app.hosts.push(profile);

        let _ = update_sidebar(&mut app, SidebarMessage::EditHost(id));

        assert_eq!(app.sidebar.editing_id, Some(id));
        assert_eq!(app.sidebar.name_input, "Work");
    }

    #[test]
    fn cancel_edit_resets_the_form() {
        let mut app = test_app();
        app.sidebar.editing_id = Some(HostId::new());
        app.sidebar.name_input = "Work".to_string();

        let _ = update_sidebar(&mut app, SidebarMessage::CancelEdit);

        assert_eq!(app.sidebar.editing_id, None);
        assert!(app.sidebar.name_input.is_empty());
    }

    #[test]
    fn save_host_is_a_no_op_when_name_is_empty() {
        let mut app = test_app();
        app.sidebar.address_input = "example.com".to_string();

        let _ = update_sidebar(&mut app, SidebarMessage::SaveHost);

        // The form is untouched: a real save always clears it.
        assert_eq!(app.sidebar.address_input, "example.com");
    }

    #[test]
    fn select_host_opens_a_new_tab_and_sends_connect_to_the_ssh_worker() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let profile = HostProfile::new("Work", "example.com", "alice");
        let id = profile.id;
        app.hosts.push(profile);
        let tab_count_before = app.tabs.len();

        let _ = update_sidebar(&mut app, SidebarMessage::SelectHost(id));

        assert_eq!(app.tabs.len(), tab_count_before + 1);
        let new_tab = app.tabs.last().expect("a tab was pushed");
        assert_eq!(app.active_tab, Some(new_tab.id));
        assert_eq!(new_tab.title, "Work");
        assert_eq!(new_tab.status, ConnectionStatus::Connecting);

        match rx.try_recv() {
            Ok(SshWorkerInput::Connect(tab_id, profile)) => {
                assert_eq!(tab_id, new_tab.id);
                assert_eq!(profile.id, id);
            }
            other => panic!("expected a Connect for the selected host, got {other:?}"),
        }
    }

    #[test]
    fn import_from_ssh_config_reports_when_there_is_nothing_new() {
        let mut app = test_app();
        app.ssh_config = SshConfig::parse("Host work\n\tHostName gitlab.example.com\n").unwrap();
        app.hosts
            .push(HostProfile::new("work", "gitlab.example.com", "deploy"));

        let _ = update_sidebar(&mut app, SidebarMessage::ImportFromSshConfig);

        assert!(active_grid_rows(&app)
            .iter()
            .any(|row| row.contains("no new hosts")));
    }

    #[test]
    fn import_from_ssh_config_reports_progress_for_new_aliases() {
        let mut app = test_app();
        app.ssh_config =
            SshConfig::parse("Host work\n\tHostName gitlab.internal.example.com\n").unwrap();

        let _ = update_sidebar(&mut app, SidebarMessage::ImportFromSshConfig);

        assert!(active_grid_rows(&app)
            .iter()
            .any(|row| row.contains("importing 1 host")));
    }

    #[test]
    fn generate_key_message_refuses_to_overwrite_an_existing_file() {
        let mut app = test_app();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        std::fs::write(&path, b"already here").unwrap();
        app.sidebar.key_path_input = path.display().to_string();

        let _ = update_sidebar(&mut app, SidebarMessage::GenerateKey);

        // Refused, so the existing file's contents are untouched and the
        // key-path field wasn't cleared/replaced by this attempt.
        assert_eq!(std::fs::read(&path).unwrap(), b"already here");
        assert!(active_grid_rows(&app)
            .iter()
            .any(|row| row.contains("already exists")));
    }

    #[test]
    fn generate_key_message_success_fills_key_path_and_switches_auth_kind() {
        let mut app = test_app();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519");
        app.sidebar.key_path_input = path.display().to_string();
        app.sidebar.auth_kind = AuthKind::Agent;

        let _ = update_sidebar(&mut app, SidebarMessage::GenerateKey);

        assert!(path.exists());
        assert_eq!(app.sidebar.auth_kind, AuthKind::PublicKey);
        assert_eq!(app.sidebar.key_path_input, path.display().to_string());
    }

    // ── M5: tabs ────────────────────────────────────────────────────────────

    /// Pushes an SSH-backed tab directly (bypassing `SelectHost`/the worker)
    /// and makes it active, for tests that only care about tab-lifecycle
    /// logic, not the connect handshake.
    fn push_ssh_tab(app: &mut TermiteApp, session_id: Option<SessionId>) -> TabId {
        let profile = HostProfile::new("Work", "example.com", "alice");
        let tab = Tab::new(
            profile.name.clone(),
            TabKind::Ssh {
                session_id,
                profile,
                reconnect_attempt: 0,
            },
            ConnectionStatus::Connected,
            ROWS,
            COLS,
        );
        let id = tab.id;
        app.tabs.push(tab);
        app.active_tab = Some(id);
        id
    }

    #[test]
    fn new_local_tab_message_adds_and_activates_a_tab() {
        let mut app = test_app();
        let tab_count_before = app.tabs.len();

        let _ = update(&mut app, Message::NewLocalTab);

        assert_eq!(app.tabs.len(), tab_count_before + 1);
        assert_eq!(app.active_tab, app.tabs.last().map(|tab| tab.id));
    }

    #[test]
    fn close_tab_activates_a_neighbor_when_the_active_tab_is_closed() {
        let mut app = test_app();
        let first = app.active_tab.expect("a starting tab");
        let _ = update(&mut app, Message::NewLocalTab);
        let second = app.active_tab.expect("the new tab is active");
        assert_ne!(first, second);

        let _ = close_tab(&mut app, second);

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab, Some(first));
    }

    #[test]
    fn close_tab_on_the_only_tab_reopens_a_fresh_local_tab() {
        let mut app = test_app();
        let only = app.active_tab.expect("a starting tab");

        let _ = close_tab(&mut app, only);

        assert_eq!(app.tabs.len(), 1);
        assert_ne!(app.active_tab, Some(only));
    }

    #[test]
    fn close_tab_disconnects_a_live_ssh_session() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let session_id = SessionId::new();
        let tab_id = push_ssh_tab(&mut app, Some(session_id));

        let _ = close_tab(&mut app, tab_id);

        match rx.try_recv() {
            Ok(SshWorkerInput::Send(id, SessionCommand::Disconnect)) => {
                assert_eq!(id, session_id);
            }
            other => panic!("expected a Disconnect for the closed tab's session, got {other:?}"),
        }
    }

    #[test]
    fn close_tab_clears_a_pending_prompt_for_its_session() {
        let mut app = test_app();
        let session_id = SessionId::new();
        let tab_id = push_ssh_tab(&mut app, Some(session_id));
        let _ = handle_session_event(
            &mut app,
            session_id,
            SessionEvent::AuthRequired(test_password_challenge()),
        );
        assert!(app.pending_prompt.is_some());

        let _ = close_tab(&mut app, tab_id);

        assert!(app.pending_prompt.is_none());
    }

    #[test]
    fn session_spawned_fills_in_the_tabs_session_id() {
        let mut app = test_app();
        let tab_id = push_ssh_tab(&mut app, None);
        let session_id = SessionId::new();

        let _ = update(&mut app, Message::SessionSpawned(tab_id, session_id));

        match &app.find_tab(tab_id).expect("tab still exists").kind {
            TabKind::Ssh { session_id: id, .. } => assert_eq!(*id, Some(session_id)),
            TabKind::Local { .. } => panic!("expected an SSH tab"),
        }
    }

    #[test]
    fn disconnected_with_a_non_requested_reason_schedules_a_reconnect() {
        let mut app = test_app();
        let session_id = SessionId::new();
        let tab_id = push_ssh_tab(&mut app, Some(session_id));

        let _ = handle_session_event(
            &mut app,
            session_id,
            SessionEvent::Disconnected {
                reason: DisconnectReason::Remote,
            },
        );

        let tab = app.find_tab(tab_id).expect("tab still exists");
        assert_eq!(tab.status, ConnectionStatus::Reconnecting { attempt: 1 });
        assert_eq!(tab.kind.session_id(), None);
    }

    #[test]
    fn disconnected_with_a_requested_reason_does_not_reconnect() {
        let mut app = test_app();
        let session_id = SessionId::new();
        let tab_id = push_ssh_tab(&mut app, Some(session_id));

        let _ = handle_session_event(
            &mut app,
            session_id,
            SessionEvent::Disconnected {
                reason: DisconnectReason::Requested,
            },
        );

        let tab = app.find_tab(tab_id).expect("tab still exists");
        assert_eq!(tab.status, ConnectionStatus::Disconnected);
    }

    #[test]
    fn disconnected_stops_auto_reconnecting_past_the_attempt_cap() {
        let mut app = test_app();
        let session_id = SessionId::new();
        let tab_id = push_ssh_tab(&mut app, Some(session_id));
        if let TabKind::Ssh {
            reconnect_attempt, ..
        } = &mut app.find_tab_mut(tab_id).unwrap().kind
        {
            *reconnect_attempt = MAX_RECONNECT_ATTEMPTS;
        }

        let _ = handle_session_event(
            &mut app,
            session_id,
            SessionEvent::Disconnected {
                reason: DisconnectReason::Remote,
            },
        );

        let tab = app.find_tab(tab_id).expect("tab still exists");
        assert_eq!(tab.status, ConnectionStatus::Disconnected);
    }

    #[test]
    fn reconnect_sends_connect_for_the_tabs_stored_profile() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let tab_id = push_ssh_tab(&mut app, None);

        let _ = reconnect(&mut app, tab_id);

        assert_eq!(
            app.find_tab(tab_id).unwrap().status,
            ConnectionStatus::Connecting
        );
        match rx.try_recv() {
            Ok(SshWorkerInput::Connect(id, profile)) => {
                assert_eq!(id, tab_id);
                assert_eq!(profile.name, "Work");
            }
            other => panic!("expected a Connect for the reconnecting tab, got {other:?}"),
        }
    }

    #[test]
    fn select_adjacent_tab_wraps_around_in_both_directions() {
        let mut app = test_app();
        let first = app.active_tab.unwrap();
        let _ = update(&mut app, Message::NewLocalTab);
        let second = app.active_tab.unwrap();
        let _ = update(&mut app, Message::NewLocalTab);
        let third = app.active_tab.unwrap();
        app.active_tab = Some(first);

        select_adjacent_tab(&mut app, 1);
        assert_eq!(app.active_tab, Some(second));
        select_adjacent_tab(&mut app, 1);
        assert_eq!(app.active_tab, Some(third));
        select_adjacent_tab(&mut app, 1);
        assert_eq!(app.active_tab, Some(first), "should wrap back to the start");

        select_adjacent_tab(&mut app, -1);
        assert_eq!(app.active_tab, Some(third), "should wrap backward too");
    }

    #[test]
    fn select_tab_by_index_ignores_an_out_of_range_index() {
        let mut app = test_app();
        let only = app.active_tab.unwrap();

        select_tab_by_index(&mut app, 5);

        assert_eq!(app.active_tab, Some(only));
    }

    #[test]
    fn ctrl_tab_shortcut_switches_the_active_tab() {
        let mut app = test_app();
        let first = app.active_tab.unwrap();
        let _ = update(&mut app, Message::NewLocalTab);
        let second = app.active_tab.unwrap();
        app.active_tab = Some(first);

        let task = handle_tab_shortcut(&mut app, &Key::Named(Named::Tab), Modifiers::CTRL);

        assert!(task.is_some());
        assert_eq!(app.active_tab, Some(second));
    }

    #[test]
    fn ctrl_digit_shortcut_selects_tab_by_position() {
        let mut app = test_app();
        let first = app.active_tab.unwrap();
        let _ = update(&mut app, Message::NewLocalTab);

        let task = handle_tab_shortcut(&mut app, &Key::Character("1".into()), Modifiers::CTRL);

        assert!(task.is_some());
        assert_eq!(app.active_tab, Some(first));
    }

    #[test]
    fn plain_tab_key_is_not_claimed_as_a_shortcut() {
        let mut app = test_app();

        let task = handle_tab_shortcut(&mut app, &Key::Named(Named::Tab), Modifiers::empty());

        assert!(task.is_none());
    }

    // ── M6: advanced terminal features ────────────────────────────────────

    /// `+ 0.5` cells of slack on each dimension so the assertion is robust
    /// to `f32` rounding in the pixels-to-cells division — `floor` still
    /// lands on the intended integer either way.
    fn window_size_for(cols: usize, rows: usize) -> iced::Size {
        iced::Size::new(
            SIDEBAR_WIDTH_PX + CELL_WIDTH_PX * (cols as f32 + 0.5),
            CHROME_HEIGHT_PX + CELL_HEIGHT_PX * (rows as f32 + 0.5),
        )
    }

    #[test]
    fn grid_size_for_window_computes_rows_and_cols_from_pixels() {
        assert_eq!(grid_size_for_window(window_size_for(80, 24)), (24, 80));
    }

    #[test]
    fn grid_size_for_window_clamps_to_at_least_one_by_one() {
        assert_eq!(grid_size_for_window(iced::Size::new(10.0, 10.0)), (1, 1));
    }

    #[test]
    fn window_resized_updates_grid_size_and_resizes_open_tabs() {
        let mut app = test_app();

        let _ = update(&mut app, Message::WindowResized(window_size_for(50, 20)));

        assert_eq!(app.grid_size, (20, 50));
        let tab = app.active_tab().expect("an active tab");
        assert_eq!((tab.grid.rows(), tab.grid.cols()), (20, 50));
    }

    #[test]
    fn new_local_tab_after_resize_matches_the_new_size() {
        let mut app = test_app();
        let _ = update(&mut app, Message::WindowResized(window_size_for(40, 15)));

        let _ = update(&mut app, Message::NewLocalTab);

        let tab = app.active_tab().expect("an active tab");
        assert_eq!((tab.grid.rows(), tab.grid.cols()), (15, 40));
    }

    #[test]
    fn ssh_tab_resize_sends_a_resize_command() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let session_id = SessionId::new();
        push_ssh_tab(&mut app, Some(session_id));

        let _ = update(&mut app, Message::WindowResized(window_size_for(60, 30)));

        let mut saw_resize = false;
        while let Ok(input) = rx.try_recv() {
            if let SshWorkerInput::Send(id, SessionCommand::Resize { rows, cols }) = input {
                assert_eq!(id, session_id);
                assert_eq!((rows, cols), (30, 60));
                saw_resize = true;
            }
        }
        assert!(saw_resize, "expected a Resize command for the ssh tab");
    }

    #[test]
    fn bell_byte_sets_flash_and_bell_timeout_clears_it() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let session_id = SessionId::new();
        let tab_id = push_ssh_tab(&mut app, Some(session_id));

        let _ = handle_session_event(&mut app, session_id, SessionEvent::Output(vec![0x07]));
        assert!(app.find_tab(tab_id).unwrap().bell_flash);

        let _ = update(&mut app, Message::BellTimeout(tab_id));
        assert!(!app.find_tab(tab_id).unwrap().bell_flash);

        while rx.try_recv().is_ok() {}
    }

    #[test]
    fn bell_timeout_for_a_closed_tab_is_a_no_op() {
        let mut app = test_app();
        let closed_tab_id = TabId::new();

        let _ = update(&mut app, Message::BellTimeout(closed_tab_id));

        assert!(app.find_tab(closed_tab_id).is_none());
    }

    #[test]
    fn is_paste_shortcut_requires_ctrl_shift_v() {
        let ctrl_shift = Modifiers::CTRL | Modifiers::SHIFT;
        assert!(is_paste_shortcut(&Key::Character("v".into()), ctrl_shift));
        assert!(is_paste_shortcut(&Key::Character("V".into()), ctrl_shift));
        assert!(!is_paste_shortcut(
            &Key::Character("v".into()),
            Modifiers::CTRL
        ));
        assert!(!is_paste_shortcut(&Key::Character("a".into()), ctrl_shift));
    }

    #[test]
    fn bracketed_paste_wraps_pasted_text_for_an_ssh_tab() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let session_id = SessionId::new();
        let tab_id = push_ssh_tab(&mut app, Some(session_id));
        app.find_tab_mut(tab_id)
            .unwrap()
            .grid
            .set_bracketed_paste(true);

        paste_into_active_tab(&mut app, "hello");

        match rx.try_recv() {
            Ok(SshWorkerInput::Send(id, SessionCommand::Write(bytes))) => {
                assert_eq!(id, session_id);
                assert_eq!(bytes, b"\x1b[200~hello\x1b[201~".to_vec());
            }
            other => panic!("expected a bracketed Write command, got {other:?}"),
        }
    }

    #[test]
    fn unbracketed_paste_sends_text_unwrapped_for_an_ssh_tab() {
        let mut app = test_app();
        let mut rx = wire_fake_worker(&mut app);
        let session_id = SessionId::new();
        push_ssh_tab(&mut app, Some(session_id));

        paste_into_active_tab(&mut app, "hello");

        match rx.try_recv() {
            Ok(SshWorkerInput::Send(id, SessionCommand::Write(bytes))) => {
                assert_eq!(id, session_id);
                assert_eq!(bytes, b"hello".to_vec());
            }
            other => panic!("expected an unwrapped Write command, got {other:?}"),
        }
    }

    // ── M6: mouse reporting ─────────────────────────────────────────────────

    #[test]
    fn cell_at_converts_pixels_to_1_indexed_cells_and_clamps_to_grid_bounds() {
        let origin = iced::Point::new(0.0, 0.0);
        assert_eq!(cell_at(origin, 24, 80), (1, 1));

        let mid = iced::Point::new(CELL_WIDTH_PX * 4.5, CELL_HEIGHT_PX * 2.5);
        assert_eq!(cell_at(mid, 24, 80), (5, 3));

        let past_the_edge = iced::Point::new(CELL_WIDTH_PX * 1000.0, CELL_HEIGHT_PX * 1000.0);
        assert_eq!(
            cell_at(past_the_edge, 24, 80),
            (80, 24),
            "should clamp to the grid's own bounds, not report an out-of-range cell"
        );
    }

    #[test]
    fn encode_mouse_event_sgr_uses_the_m_case_to_distinguish_press_from_release() {
        assert_eq!(
            encode_mouse_event(0, false, 5, 3, true),
            b"\x1b[<0;5;3M".to_vec()
        );
        assert_eq!(
            encode_mouse_event(0, true, 5, 3, true),
            b"\x1b[<0;5;3m".to_vec()
        );
    }

    #[test]
    fn encode_mouse_event_legacy_offsets_by_32_and_cant_name_the_released_button() {
        // Left press (code 0) at col 1, row 1: CSI M, then (0+32), (1+32), (1+32).
        assert_eq!(
            encode_mouse_event(0, false, 1, 1, false),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
        // Any release becomes the fixed "no button" code, not the pressed button's code.
        assert_eq!(
            encode_mouse_event(0, true, 1, 1, false),
            vec![0x1b, b'[', b'M', MOUSE_NO_BUTTON + 32, 33, 33]
        );
    }

    #[test]
    fn encode_mouse_event_legacy_clamps_coordinates_at_223() {
        let bytes = encode_mouse_event(0, false, 9999, 9999, false);
        assert_eq!(bytes[4], 223 + 32);
        assert_eq!(bytes[5], 223 + 32);
    }

    /// Sets up an SSH tab as the active tab with `mode`/`sgr` mouse tracking
    /// already enabled on its grid, and a fake worker channel to inspect
    /// what gets sent.
    fn tab_with_mouse_tracking(
        mode: MouseTracking,
        sgr: bool,
    ) -> (TermiteApp, bridge::Receiver<SshWorkerInput>, SessionId) {
        let mut app = test_app();
        let rx = wire_fake_worker(&mut app);
        let session_id = SessionId::new();
        let tab_id = push_ssh_tab(&mut app, Some(session_id));
        let tab = app.find_tab_mut(tab_id).unwrap();
        tab.grid.set_mouse_tracking(mode);
        tab.grid.set_mouse_sgr(sgr);
        (app, rx, session_id)
    }

    #[test]
    fn mouse_press_and_release_report_under_normal_tracking() {
        let (mut app, mut rx, session_id) = tab_with_mouse_tracking(MouseTracking::Normal, true);
        let point = iced::Point::new(0.0, 0.0);

        let _ = update(&mut app, Message::MouseMoved(point));
        let _ = update(&mut app, Message::MousePress(iced::mouse::Button::Left));
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(id, SessionCommand::Write(bytes))) => {
                assert_eq!(id, session_id);
                assert_eq!(bytes, b"\x1b[<0;1;1M".to_vec());
            }
            other => panic!("expected a press report, got {other:?}"),
        }

        let _ = update(&mut app, Message::MouseRelease(iced::mouse::Button::Left));
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(id, SessionCommand::Write(bytes))) => {
                assert_eq!(id, session_id);
                assert_eq!(bytes, b"\x1b[<0;1;1m".to_vec());
            }
            other => panic!("expected a release report, got {other:?}"),
        }
    }

    #[test]
    fn mouse_events_are_a_no_op_when_tracking_is_off() {
        let (mut app, mut rx, _session_id) = tab_with_mouse_tracking(MouseTracking::Off, true);

        let _ = update(&mut app, Message::MouseMoved(iced::Point::new(0.0, 0.0)));
        let _ = update(&mut app, Message::MousePress(iced::mouse::Button::Left));
        let _ = update(&mut app, Message::MouseRelease(iced::mouse::Button::Left));

        assert!(
            rx.try_recv().is_err(),
            "no mouse report should be sent while tracking is off"
        );
    }

    #[test]
    fn plain_motion_is_only_reported_under_any_event_tracking() {
        let point = iced::Point::new(0.0, 0.0);

        let (mut app, mut rx, _) = tab_with_mouse_tracking(MouseTracking::ButtonEvent, true);
        let _ = update(&mut app, Message::MouseMoved(point));
        assert!(
            rx.try_recv().is_err(),
            "ButtonEvent tracking shouldn't report motion with no button held"
        );

        let (mut app, mut rx, _) = tab_with_mouse_tracking(MouseTracking::AnyEvent, true);
        let _ = update(&mut app, Message::MouseMoved(point));
        assert!(
            rx.try_recv().is_ok(),
            "AnyEvent tracking should report plain motion"
        );
    }

    #[test]
    fn drag_motion_is_reported_under_button_event_tracking() {
        let (mut app, mut rx, _) = tab_with_mouse_tracking(MouseTracking::ButtonEvent, true);
        let point = iced::Point::new(0.0, 0.0);

        let _ = update(&mut app, Message::MousePress(iced::mouse::Button::Left));
        while rx.try_recv().is_ok() {}
        let _ = update(&mut app, Message::MouseMoved(point));

        match rx.try_recv() {
            Ok(SshWorkerInput::Send(_, SessionCommand::Write(bytes))) => {
                assert_eq!(bytes, b"\x1b[<32;1;1M".to_vec(), "0 (left) + 32 (motion)");
            }
            other => panic!("expected a drag-motion report, got {other:?}"),
        }
    }

    #[test]
    fn wheel_scroll_reports_up_or_down_by_sign() {
        let (mut app, mut rx, _) = tab_with_mouse_tracking(MouseTracking::Normal, true);
        let _ = update(&mut app, Message::MouseMoved(iced::Point::new(0.0, 0.0)));

        let _ = update(
            &mut app,
            Message::MouseScrolled(iced::mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 }),
        );
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(_, SessionCommand::Write(bytes))) => {
                assert_eq!(bytes, b"\x1b[<64;1;1M".to_vec(), "wheel up");
            }
            other => panic!("expected a wheel-up report, got {other:?}"),
        }

        let _ = update(
            &mut app,
            Message::MouseScrolled(iced::mouse::ScrollDelta::Lines { x: 0.0, y: -1.0 }),
        );
        match rx.try_recv() {
            Ok(SshWorkerInput::Send(_, SessionCommand::Write(bytes))) => {
                assert_eq!(bytes, b"\x1b[<65;1;1M".to_vec(), "wheel down");
            }
            other => panic!("expected a wheel-down report, got {other:?}"),
        }
    }

    // ── M6: selection ─────────────────────────────────────────────────────

    #[test]
    fn word_bounds_expands_to_the_run_of_word_or_non_word_chars() {
        let row: Vec<char> = "foo  bar-baz".chars().collect();
        assert_eq!(word_bounds(&row, 1), (0, 2), "inside `foo`");
        assert_eq!(word_bounds(&row, 3), (3, 4), "the run of spaces");
        assert_eq!(word_bounds(&row, 6), (5, 7), "inside `bar`");
        assert_eq!(word_bounds(&row, 8), (8, 8), "a lone punctuation run");
    }

    #[test]
    fn selection_range_character_mode_normalizes_drag_direction() {
        let rows = vec!["abcdef".chars().collect::<Vec<char>>()];
        let forward = Selection {
            anchor: (0, 1),
            head: (0, 4),
            mode: SelectionMode::Character,
        };
        let backward = Selection {
            anchor: (0, 4),
            head: (0, 1),
            mode: SelectionMode::Character,
        };
        assert_eq!(selection_range(&rows, &forward), ((0, 1), (0, 4)));
        assert_eq!(selection_range(&rows, &backward), ((0, 1), (0, 4)));
    }

    #[test]
    fn selection_range_word_mode_expands_both_ends_to_word_boundaries() {
        let rows = vec!["foo bar baz".chars().collect::<Vec<char>>()];
        let selection = Selection {
            anchor: (0, 1), // inside "foo"
            head: (0, 9),   // inside "baz"
            mode: SelectionMode::Word,
        };
        assert_eq!(selection_range(&rows, &selection), ((0, 0), (0, 10)));
    }

    #[test]
    fn selection_range_line_mode_covers_the_full_row_width() {
        let rows = vec!["ab".chars().collect::<Vec<char>>(), "cd".chars().collect()];
        let selection = Selection {
            anchor: (0, 1),
            head: (1, 0),
            mode: SelectionMode::Line,
        };
        assert_eq!(selection_range(&rows, &selection), ((0, 0), (1, 1)));
    }

    #[test]
    fn selection_text_trims_padding_and_joins_multiple_rows() {
        let rows = vec![
            "hello   ".chars().collect::<Vec<char>>(),
            "world   ".chars().collect(),
        ];
        let selection = Selection {
            anchor: (0, 0),
            head: (1, 4),
            mode: SelectionMode::Character,
        };
        assert_eq!(selection_text(&rows, &selection), "hello\nworld");
    }

    #[test]
    fn click_selection_mode_cycles_character_word_line_on_the_same_cell() {
        let mut app = test_app();
        let cell = (2, 3);
        assert_eq!(
            click_selection_mode(&mut app, cell),
            SelectionMode::Character
        );
        assert_eq!(click_selection_mode(&mut app, cell), SelectionMode::Word);
        assert_eq!(click_selection_mode(&mut app, cell), SelectionMode::Line);
        assert_eq!(
            click_selection_mode(&mut app, cell),
            SelectionMode::Character,
            "a fourth click cycles back to character"
        );
    }

    #[test]
    fn click_selection_mode_resets_on_a_different_cell() {
        let mut app = test_app();
        assert_eq!(
            click_selection_mode(&mut app, (0, 0)),
            SelectionMode::Character
        );
        assert_eq!(
            click_selection_mode(&mut app, (1, 1)),
            SelectionMode::Character,
            "a click on a different cell doesn't chain"
        );
    }

    #[test]
    fn mouse_drag_starts_and_extends_a_character_selection() {
        let mut app = test_app();
        let start = iced::Point::new(0.0, 0.0);
        let end = iced::Point::new(CELL_WIDTH_PX * 3.5, CELL_HEIGHT_PX * 1.5);

        let _ = update(&mut app, Message::MouseMoved(start));
        let _ = update(&mut app, Message::MousePress(iced::mouse::Button::Left));
        let _ = update(&mut app, Message::MouseMoved(end));

        let selection = app.active_tab().unwrap().selection.unwrap();
        assert_eq!(selection.mode, SelectionMode::Character);
        assert_eq!(selection.anchor, (0, 0));
        assert_eq!(selection.head, (1, 3));
    }

    #[test]
    fn releasing_a_plain_click_with_no_drag_clears_the_selection() {
        let mut app = test_app();
        let point = iced::Point::new(0.0, 0.0);

        let _ = update(&mut app, Message::MouseMoved(point));
        let _ = update(&mut app, Message::MousePress(iced::mouse::Button::Left));
        let _ = update(&mut app, Message::MouseRelease(iced::mouse::Button::Left));

        assert!(app.active_tab().unwrap().selection.is_none());
    }

    #[test]
    fn releasing_after_a_drag_keeps_the_selection() {
        let mut app = test_app();
        let start = iced::Point::new(0.0, 0.0);
        let end = iced::Point::new(CELL_WIDTH_PX * 3.5, 0.0);

        let _ = update(&mut app, Message::MouseMoved(start));
        let _ = update(&mut app, Message::MousePress(iced::mouse::Button::Left));
        let _ = update(&mut app, Message::MouseMoved(end));
        let _ = update(&mut app, Message::MouseRelease(iced::mouse::Button::Left));

        assert!(app.active_tab().unwrap().selection.is_some());
    }

    #[test]
    fn a_double_click_selects_word_mode_even_without_a_drag() {
        let mut app = test_app();
        let point = iced::Point::new(0.0, 0.0);

        let _ = update(&mut app, Message::MouseMoved(point));
        let _ = update(&mut app, Message::MousePress(iced::mouse::Button::Left));
        let _ = update(&mut app, Message::MouseRelease(iced::mouse::Button::Left));
        let _ = update(&mut app, Message::MousePress(iced::mouse::Button::Left));

        let selection = app.active_tab().unwrap().selection.unwrap();
        assert_eq!(selection.mode, SelectionMode::Word);

        let _ = update(&mut app, Message::MouseRelease(iced::mouse::Button::Left));
        assert!(
            app.active_tab().unwrap().selection.is_some(),
            "a word-mode selection survives release even with anchor == head"
        );
    }

    #[test]
    fn selection_is_not_started_while_mouse_tracking_is_enabled() {
        let mut app = test_app();
        app.active_tab_mut()
            .unwrap()
            .grid
            .set_mouse_tracking(MouseTracking::Normal);
        let point = iced::Point::new(0.0, 0.0);

        let _ = update(&mut app, Message::MouseMoved(point));
        let _ = update(&mut app, Message::MousePress(iced::mouse::Button::Left));

        assert!(app.active_tab().unwrap().selection.is_none());
    }

    #[test]
    fn terminal_spans_with_no_selection_is_one_plain_span_per_row() {
        let rows = vec!["ab".chars().collect::<Vec<char>>(), "cd".chars().collect()];

        let spans = terminal_spans(&rows, None);

        assert_eq!(spans.len(), 3, "two rows plus one newline separator");
        assert_eq!(spans[0].text.as_ref(), "ab");
        assert!(spans[0].highlight.is_none());
        assert_eq!(spans[1].text.as_ref(), "\n");
        assert_eq!(spans[2].text.as_ref(), "cd");
    }

    #[test]
    fn terminal_spans_splits_a_partially_selected_row_into_three_spans() {
        let rows = vec!["abcd".chars().collect::<Vec<char>>()];
        let selection = Selection {
            anchor: (0, 1),
            head: (0, 2),
            mode: SelectionMode::Character,
        };

        let spans = terminal_spans(&rows, Some(&selection));

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text.as_ref(), "a");
        assert!(spans[0].highlight.is_none());
        assert_eq!(spans[1].text.as_ref(), "bc");
        assert!(spans[1].highlight.is_some());
        assert_eq!(spans[2].text.as_ref(), "d");
        assert!(spans[2].highlight.is_none());
    }

    #[test]
    fn terminal_spans_for_a_fully_selected_row_is_a_single_highlighted_span() {
        let rows = vec!["abcd".chars().collect::<Vec<char>>()];
        let selection = Selection {
            anchor: (0, 0),
            head: (0, 3),
            mode: SelectionMode::Line,
        };

        let spans = terminal_spans(&rows, Some(&selection));

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text.as_ref(), "abcd");
        assert!(spans[0].highlight.is_some());
    }
}
