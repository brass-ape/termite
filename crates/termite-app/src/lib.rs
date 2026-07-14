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

use termite_core::{AuthMethod, CredentialStore, HostProfile, SessionId, TermiteError};
use termite_ssh::{AuthChallenge, AuthResponse, HostKey, SessionCommand, SessionEvent, SshSession};
use termite_storage::{HostStore, KeyringStore, MemoryHostStore, TomlHostStore};
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
    /// OS keychain access for the credential prompt's "Save to keychain"
    /// toggle (see `saved_credential`/`save_credential`).
    credential_store: Arc<dyn CredentialStore>,

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
            credential_store: Arc::new(KeyringStore::new()),
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
            if let Some(secret) = saved_credential(app, &challenge) {
                let response = match &challenge {
                    AuthChallenge::Password { .. } => AuthResponse::Password(secret),
                    AuthChallenge::Passphrase { .. } => AuthResponse::Passphrase(secret),
                };
                send_to_session(app, id, SessionCommand::AuthResponse(response));
                return;
            }
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
                profile.auth = auth_method_from_form(
                    app.sidebar.auth_kind,
                    std::mem::take(&mut app.sidebar.key_path_input),
                );
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

        handle_session_event(
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

        handle_session_event(
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

        handle_session_event(&mut app, id, SessionEvent::HostKeyUnknown(key.clone()));

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

        handle_session_event(&mut app, id, SessionEvent::HostKeyMismatch(test_host_key()));

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

        handle_session_event(
            &mut app,
            first,
            SessionEvent::AuthRequired(test_password_challenge()),
        );
        handle_session_event(
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

        handle_session_event(
            &mut app,
            first,
            SessionEvent::HostKeyUnknown(test_host_key()),
        );
        handle_session_event(
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

        handle_session_event(
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

        handle_session_event(
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

        handle_session_event(
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

        handle_session_event(
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

        handle_session_event(
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

        handle_session_event(
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

        handle_session_event(&mut app, id, SessionEvent::HostKeyUnknown(test_host_key()));
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

        handle_session_event(&mut app, id, SessionEvent::HostKeyMismatch(test_host_key()));
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
        handle_session_event(
            &mut app,
            id,
            SessionEvent::AuthRequired(test_password_challenge()),
        );
        assert!(app.pending_prompt.is_some());

        handle_session_event(
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
        handle_session_event(
            &mut app,
            prompting,
            SessionEvent::AuthRequired(test_password_challenge()),
        );

        handle_session_event(
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
}
