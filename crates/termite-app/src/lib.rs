// SPDX-License-Identifier: MIT
//! Top-level application state and Iced wiring for Termite.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::futures::channel::mpsc as bridge;
use iced::futures::{SinkExt, Stream, StreamExt};
use iced::keyboard::key::Named;
use iced::keyboard::{Key, Modifiers};
use iced::widget::{row, text, Stack};
use iced::{stream, Element, Font, Length, Subscription, Task, Theme};
use secrecy::SecretString;

use termite_core::{AuthMethod, HostProfile, SessionId};
use termite_ssh::{AuthChallenge, AuthResponse, HostKey, SessionCommand, SessionEvent, SshSession};
use termite_storage::{HostStore, MemoryHostStore, TomlHostStore};
use termite_terminal::{GridHandler, Pty, TerminalGrid};
use termite_ui::{prompt, sidebar, AuthKind, Prompt, PromptMessage, SidebarMessage, SidebarState};

/// Default grid size until real window-size-driven resizing lands (M6).
const ROWS: usize = 30;
const COLS: usize = 100;

/// How often the output buffer is drained and fed to the VT parser.
const POLL_INTERVAL: Duration = Duration::from_millis(16);

// ── Application state ─────────────────────────────────────────────────────────

/// Root application state.
///
/// Extended in M1+ as sessions, host profiles, and UI state are added.
pub struct TermiteApp {
    grid: TerminalGrid,
    parser: vte::Parser,
    output: Arc<Mutex<Vec<u8>>>,
    writer: Box<dyn Write + Send>,
    /// Kept alive so the pty and child shell aren't torn down; unused
    /// otherwise until session lifecycle (M2+) needs it.
    _pty: Pty,

    // ── M4: host management ──────────────────────────────────────────
    host_store: Arc<dyn HostStore>,
    hosts: Vec<HostProfile>,
    sidebar: SidebarState,

    // ── M4: SSH session wiring ────────────────────────────────────────
    /// Sender into the persistent SSH worker subscription; `None` until its
    /// first poll delivers `Message::SshWorkerReady`.
    ssh_worker: Option<bridge::Sender<SshWorkerInput>>,
    /// The session currently receiving keystrokes and rendering into
    /// `grid`, if any. `None` means the local shell PTY has focus.
    active_session: Option<SessionId>,
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
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
        let pty = Pty::spawn(&shell, ROWS as u16, COLS as u16)?;

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

        Ok(Self {
            grid: TerminalGrid::new(ROWS, COLS),
            parser: vte::Parser::new(),
            output,
            writer,
            _pty: pty,
            host_store: make_host_store(),
            hosts: Vec::new(),
            sidebar: SidebarState::default(),
            ssh_worker: None,
            active_session: None,
            pending_prompt: None,
        })
    }

    /// Feeds raw PTY output bytes through the VT parser into the grid.
    fn advance(&mut self, bytes: &[u8]) {
        let parser = &mut self.parser;
        let grid = &mut self.grid;
        let mut handler = GridHandler { grid };
        for &byte in bytes {
            parser.advance(&mut handler, byte);
        }
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

/// Requests sent from the app to the persistent SSH worker subscription
/// (see [`ssh_worker`]).
#[derive(Debug, Clone)]
pub enum SshWorkerInput {
    /// Spawn a new session connecting to this host profile.
    Connect(HostProfile),
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
                        Some(SshWorkerInput::Connect(profile)) => {
                            match termite_ssh::known_hosts::known_hosts_path() {
                                Ok(known_hosts_path) => {
                                    let (id, command_tx) =
                                        SshSession::spawn(profile, known_hosts_path, event_tx.clone());
                                    sessions.insert(id, command_tx);
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
            let bytes = {
                match app.output.lock() {
                    Ok(mut output) => std::mem::take(&mut *output),
                    Err(_) => Vec::new(),
                }
            };
            if !bytes.is_empty() {
                app.advance(&bytes);
            }
        }
        Message::KeyPressed { key, modifiers } => {
            if let Some(bytes) = key_to_bytes(&key, modifiers) {
                match app.active_session {
                    Some(id) => send_to_session(app, id, SessionCommand::Write(bytes)),
                    None => {
                        let _ = app.writer.write_all(&bytes);
                    }
                }
            }
        }
        Message::HostsLoaded(hosts) => {
            app.hosts = hosts;
        }
        Message::Sidebar(message) => return update_sidebar(app, message),
        Message::SshWorkerReady(sender) => {
            app.ssh_worker = Some(sender);
        }
        Message::SessionEvent(id, event) => handle_session_event(app, id, event),
        Message::Prompt(message) => update_prompt(app, message),
    }
    Task::none()
}

/// Handles an event forwarded from a running SSH session. There is no
/// dedicated status UI yet (that lands with tabs in M5), so connection
/// lifecycle transitions are appended to the terminal grid as plain text —
/// the only surface currently visible to the user.
///
/// `AuthRequired` and `HostKeyUnknown`/`HostKeyMismatch` open the credential
/// or host-key modal (see `update_prompt`). Per `CLAUDE.md`'s no-silent-accept
/// invariant, only one prompt is shown at a time: if a second one arrives
/// while the modal is already open, it fails closed (auth disconnects,
/// host-key rejects) rather than silently overwriting the pending decision.
fn handle_session_event(app: &mut TermiteApp, id: SessionId, event: SessionEvent) {
    match event {
        SessionEvent::Connected => {
            app.active_session = Some(id);
            app.advance(b"\r\n*** connected ***\r\n");
        }
        SessionEvent::Output(bytes) => {
            if app.active_session == Some(id) {
                app.advance(&bytes);
            }
        }
        SessionEvent::AuthRequired(challenge) => {
            if app.pending_prompt.is_some() {
                tracing::warn!(
                    ?challenge,
                    "a prompt is already pending; disconnecting session"
                );
                send_to_session(app, id, SessionCommand::Disconnect);
                return;
            }
            let label = match &challenge {
                AuthChallenge::Password => "Password required to continue connecting".to_string(),
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
            app.advance(format!("\r\n*** disconnected: {reason:?} ***\r\n").as_bytes());
            if app.active_session == Some(id) {
                app.active_session = None;
            }
            if pending_prompt_session(&app.pending_prompt) == Some(id) {
                app.pending_prompt = None;
            }
        }
        SessionEvent::Error(message) => {
            tracing::error!(%message, "ssh session error");
            app.advance(format!("\r\n*** error: {message} ***\r\n").as_bytes());
        }
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
        PromptMessage::Submit => {
            if let Some(PendingPrompt::Credential {
                session,
                challenge,
                ui: Prompt::Credential { input, .. },
            }) = app.pending_prompt.take()
            {
                let secret = SecretString::from(input);
                let response = match challenge {
                    AuthChallenge::Password => AuthResponse::Password(secret),
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

fn update_sidebar(app: &mut TermiteApp, message: SidebarMessage) -> Task<Message> {
    match message {
        SidebarMessage::NameInputChanged(value) => {
            app.sidebar.name_input = value;
        }
        SidebarMessage::AddressInputChanged(value) => {
            app.sidebar.address_input = value;
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
        SidebarMessage::AddHost => {
            // A public-key profile with no path would fail to connect with
            // no way to fix it short of deleting and re-adding the host, so
            // it's rejected here rather than saved.
            let key_path_ok = app.sidebar.auth_kind != AuthKind::PublicKey
                || !app.sidebar.key_path_input.is_empty();
            if !app.sidebar.name_input.is_empty()
                && !app.sidebar.address_input.is_empty()
                && key_path_ok
            {
                let mut profile = HostProfile::new(
                    std::mem::take(&mut app.sidebar.name_input),
                    std::mem::take(&mut app.sidebar.address_input),
                    std::mem::take(&mut app.sidebar.username_input),
                );
                profile.auth = match app.sidebar.auth_kind {
                    AuthKind::Agent => AuthMethod::Agent,
                    AuthKind::Password => AuthMethod::Password,
                    AuthKind::PublicKey => AuthMethod::PublicKey {
                        key_path: std::mem::take(&mut app.sidebar.key_path_input).into(),
                    },
                };
                app.sidebar.auth_kind = AuthKind::default();
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
        }
        SidebarMessage::DeleteHost(id) => {
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
        SidebarMessage::SelectHost(id) => {
            if let Some(profile) = app.hosts.iter().find(|host| host.id == id).cloned() {
                match &app.ssh_worker {
                    Some(sender) => {
                        let mut sender = sender.clone();
                        let _ = sender.try_send(SshWorkerInput::Connect(profile));
                    }
                    None => tracing::warn!("ssh worker not ready yet; connect request dropped"),
                }
            }
        }
    }
    Task::none()
}

fn view(app: &TermiteApp) -> Element<'_, Message> {
    let sidebar = sidebar::view(&app.hosts, &app.sidebar).map(Message::Sidebar);

    let rows = app.grid.visible_rows().join("\n");
    let terminal = text(rows)
        .font(Font::MONOSPACE)
        .size(14)
        .width(Length::Fill);

    let content: Element<'_, Message> = row![sidebar, terminal].into();

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
