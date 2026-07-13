// SPDX-License-Identifier: MIT
//! SSH session lifecycle: connect, verify the host key, authenticate, open
//! a shell, and shuttle bytes — see `ARCHITECTURE.md` §8 for the design.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use russh::client;
use russh::ChannelMsg;
use secrecy::ExposeSecret;
use tokio::sync::mpsc;

use termite_core::{AuthMethod, HostProfile, SessionId};

use crate::events::{AuthChallenge, AuthResponse, DisconnectReason, SessionCommand, SessionEvent};
use crate::handler::SessionHandler;
use crate::signer::KeyProviderSigner;

/// Default terminal size requested for the remote PTY until the caller
/// sends a `SessionCommand::Resize` (mirrors termite-app's fixed M1 grid).
const DEFAULT_ROWS: u16 = 30;
const DEFAULT_COLS: u16 = 100;

/// Entry point for spawning SSH sessions.
pub struct SshSession;

impl SshSession {
    /// Spawns a background task that connects to `profile`'s host. Returns
    /// the newly assigned session id and the command sender used to drive
    /// it; events are delivered on `event_tx`.
    ///
    /// `known_hosts_path` is caller-supplied rather than resolved
    /// internally so tests (and future callers) can point it at an
    /// isolated file instead of the user's real `known_hosts`.
    pub fn spawn(
        profile: HostProfile,
        known_hosts_path: PathBuf,
        event_tx: mpsc::Sender<(SessionId, SessionEvent)>,
    ) -> (SessionId, mpsc::Sender<SessionCommand>) {
        let session_id = SessionId::new();
        let (command_tx, command_rx) = mpsc::channel(32);

        tokio::spawn(run(
            session_id,
            profile,
            known_hosts_path,
            event_tx,
            command_rx,
        ));

        (session_id, command_tx)
    }
}

async fn send_event(
    event_tx: &mpsc::Sender<(SessionId, SessionEvent)>,
    session_id: SessionId,
    event: SessionEvent,
) -> bool {
    event_tx.send((session_id, event)).await.is_ok()
}

async fn run(
    session_id: SessionId,
    profile: HostProfile,
    known_hosts_path: PathBuf,
    event_tx: mpsc::Sender<(SessionId, SessionEvent)>,
    mut command_rx: mpsc::Receiver<SessionCommand>,
) {
    let pending_approval = Arc::new(Mutex::new(None));
    let handler = SessionHandler {
        session_id,
        host: profile.host.clone(),
        port: profile.port,
        known_hosts_path,
        event_tx: event_tx.clone(),
        pending_approval: pending_approval.clone(),
    };

    let config = Arc::new(client::Config::default());
    let addr = (profile.host.clone(), profile.port);
    let connect_fut = client::connect(config, addr, handler);
    tokio::pin!(connect_fut);

    let mut handle = loop {
        tokio::select! {
            result = &mut connect_fut => {
                match result {
                    Ok(handle) => break handle,
                    Err(err) => {
                        send_event(
                            &event_tx,
                            session_id,
                            SessionEvent::Disconnected {
                                reason: DisconnectReason::Error(err.to_string()),
                            },
                        )
                        .await;
                        return;
                    }
                }
            }
            cmd = command_rx.recv() => {
                match cmd {
                    Some(SessionCommand::ApproveHostKey(approved)) => {
                        if let Some(tx) = pending_approval.lock().expect("pending_approval mutex poisoned").take() {
                            let _ = tx.send(approved);
                        }
                    }
                    Some(SessionCommand::Disconnect) | None => return,
                    Some(_) => {}
                }
            }
        }
    };

    if let Err(err) = authenticate(
        session_id,
        &profile,
        &mut handle,
        &event_tx,
        &mut command_rx,
    )
    .await
    {
        send_event(
            &event_tx,
            session_id,
            SessionEvent::Disconnected {
                reason: DisconnectReason::Error(err),
            },
        )
        .await;
        return;
    }

    let mut channel = match handle.channel_open_session().await {
        Ok(channel) => channel,
        Err(err) => {
            send_event(
                &event_tx,
                session_id,
                SessionEvent::Disconnected {
                    reason: DisconnectReason::Error(err.to_string()),
                },
            )
            .await;
            return;
        }
    };

    if let Err(err) = request_shell(&mut channel).await {
        send_event(
            &event_tx,
            session_id,
            SessionEvent::Disconnected {
                reason: DisconnectReason::Error(err),
            },
        )
        .await;
        return;
    }

    send_event(&event_tx, session_id, SessionEvent::Connected).await;

    let reason = shuttle(session_id, &event_tx, &mut command_rx, &mut channel).await;

    let _ = handle
        .disconnect(russh::Disconnect::ByApplication, "", "English")
        .await;
    send_event(&event_tx, session_id, SessionEvent::Disconnected { reason }).await;
}

/// Authenticates using whichever method `profile.auth` specifies.
async fn authenticate(
    session_id: SessionId,
    profile: &HostProfile,
    handle: &mut client::Handle<SessionHandler>,
    event_tx: &mpsc::Sender<(SessionId, SessionEvent)>,
    command_rx: &mut mpsc::Receiver<SessionCommand>,
) -> Result<(), String> {
    match &profile.auth {
        AuthMethod::Password => {
            authenticate_password(session_id, profile, handle, event_tx, command_rx).await
        }
        AuthMethod::PublicKey { key_path } => {
            authenticate_publickey(session_id, profile, key_path, handle, event_tx, command_rx)
                .await
        }
        AuthMethod::Agent => authenticate_agent(profile, handle).await,
    }
}

/// RSA needs the hash the server negotiated via server-sig-algs; if the
/// server never sent the extension, SHA-512 is the sensible modern default
/// (plain SHA-1 ssh-rsa is dead). Non-RSA keys have exactly one signature
/// scheme — no hash to pick.
async fn negotiated_rsa_hash(
    handle: &client::Handle<SessionHandler>,
    public_key: &russh::keys::PublicKey,
) -> Result<Option<russh::keys::HashAlg>, String> {
    if matches!(public_key.algorithm(), russh::keys::Algorithm::Rsa { .. }) {
        Ok(handle
            .best_supported_rsa_hash()
            .await
            .map_err(|err| err.to_string())?
            .unwrap_or(Some(russh::keys::HashAlg::Sha512)))
    } else {
        Ok(None)
    }
}

/// Tries every identity the user's SSH agent holds until one succeeds.
/// Signing happens inside the agent process, so private key material never
/// enters Termite at all (see `ARCHITECTURE.md` §"SSH agent integration").
/// Certificate identities are offered as their bare public key for now —
/// a server that only accepts the certificate rejects it and the next
/// identity is tried.
async fn authenticate_agent(
    profile: &HostProfile,
    handle: &mut client::Handle<SessionHandler>,
) -> Result<(), String> {
    let mut agent = crate::agent::connect().await?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|err| format!("cannot list SSH agent identities: {err}"))?;

    if identities.is_empty() {
        return Err("the SSH agent is running but holds no identities".to_string());
    }

    let tried = identities.len();
    for identity in identities {
        let public_key = identity.public_key().into_owned();
        let hash_alg = negotiated_rsa_hash(handle, &public_key).await?;

        let result = handle
            .authenticate_publickey_with(profile.username.clone(), public_key, hash_alg, &mut agent)
            .await
            .map_err(|err| err.to_string())?;
        if result.success() {
            return Ok(());
        }
    }

    Err(format!(
        "agent authentication failed for user {}: the server accepted none of \
         the agent's {tried} identities",
        profile.username
    ))
}

async fn authenticate_password(
    session_id: SessionId,
    profile: &HostProfile,
    handle: &mut client::Handle<SessionHandler>,
    event_tx: &mpsc::Sender<(SessionId, SessionEvent)>,
    command_rx: &mut mpsc::Receiver<SessionCommand>,
) -> Result<(), String> {
    if !send_event(
        event_tx,
        session_id,
        SessionEvent::AuthRequired(AuthChallenge::Password),
    )
    .await
    {
        return Err("owner dropped the event channel".to_string());
    }

    let password = loop {
        match command_rx.recv().await {
            Some(SessionCommand::AuthResponse(AuthResponse::Password(secret))) => break secret,
            Some(SessionCommand::Disconnect) | None => {
                return Err("disconnected while waiting for credentials".to_string());
            }
            Some(_) => continue,
        }
    };

    let result = handle
        .authenticate_password(
            profile.username.clone(),
            password.expose_secret().to_string(),
        )
        .await
        .map_err(|err| err.to_string())?;

    if !result.success() {
        return Err(format!(
            "authentication failed for user {}",
            profile.username
        ));
    }

    Ok(())
}

/// Loads the key at `key_path` (prompting for a passphrase if it's
/// encrypted), wraps it in a [`termite_crypto::LocalKeyProvider`], and
/// authenticates through `authenticate_publickey_with` — never the simpler
/// `authenticate_publickey`, which would hand `russh` the raw private key
/// to hold internally instead of going through the `KeyProvider`
/// abstraction (see `HANDOFF.md`'s notes on this).
async fn authenticate_publickey(
    session_id: SessionId,
    profile: &HostProfile,
    key_path: &std::path::Path,
    handle: &mut client::Handle<SessionHandler>,
    event_tx: &mpsc::Sender<(SessionId, SessionEvent)>,
    command_rx: &mut mpsc::Receiver<SessionCommand>,
) -> Result<(), String> {
    let loaded = termite_crypto::key::load(key_path).map_err(|err| err.to_string())?;

    let key = if loaded.is_encrypted() {
        let fingerprint = loaded
            .public_key()
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();

        if !send_event(
            event_tx,
            session_id,
            SessionEvent::AuthRequired(AuthChallenge::Passphrase { fingerprint }),
        )
        .await
        {
            return Err("owner dropped the event channel".to_string());
        }

        let passphrase = loop {
            match command_rx.recv().await {
                Some(SessionCommand::AuthResponse(AuthResponse::Passphrase(secret))) => {
                    break secret
                }
                Some(SessionCommand::Disconnect) | None => {
                    return Err("disconnected while waiting for a passphrase".to_string());
                }
                Some(_) => continue,
            }
        };

        termite_crypto::key::decrypt(&loaded, &passphrase).map_err(|err| err.to_string())?
    } else {
        loaded
    };

    let public_key = key.public_key().clone();
    let hash_alg = negotiated_rsa_hash(handle, &public_key).await?;

    let provider = termite_crypto::LocalKeyProvider::new(key);
    let mut signer = KeyProviderSigner::new(Box::new(provider));

    let result = handle
        .authenticate_publickey_with(profile.username.clone(), public_key, hash_alg, &mut signer)
        .await
        .map_err(|err| err.to_string())?;

    if !result.success() {
        return Err(format!(
            "authentication failed for user {}",
            profile.username
        ));
    }

    Ok(())
}

async fn request_shell(channel: &mut russh::Channel<client::Msg>) -> Result<(), String> {
    channel
        .request_pty(
            true,
            "xterm-256color",
            DEFAULT_COLS as u32,
            DEFAULT_ROWS as u32,
            0,
            0,
            &[],
        )
        .await
        .map_err(|err| err.to_string())?;
    match channel.wait().await {
        Some(ChannelMsg::Success) => {}
        other => return Err(format!("PTY request rejected: {other:?}")),
    }

    channel
        .request_shell(true)
        .await
        .map_err(|err| err.to_string())?;
    match channel.wait().await {
        Some(ChannelMsg::Success) => {}
        other => return Err(format!("shell request rejected: {other:?}")),
    }

    Ok(())
}

async fn shuttle(
    session_id: SessionId,
    event_tx: &mpsc::Sender<(SessionId, SessionEvent)>,
    command_rx: &mut mpsc::Receiver<SessionCommand>,
    channel: &mut russh::Channel<client::Msg>,
) -> DisconnectReason {
    loop {
        tokio::select! {
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        if !send_event(event_tx, session_id, SessionEvent::Output(data.to_vec())).await {
                            return DisconnectReason::Requested;
                        }
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        return DisconnectReason::Remote;
                    }
                    _ => {}
                }
            }
            cmd = command_rx.recv() => {
                match cmd {
                    Some(SessionCommand::Write(bytes)) => {
                        if let Err(err) = channel.data_bytes(bytes).await {
                            return DisconnectReason::Error(err.to_string());
                        }
                    }
                    Some(SessionCommand::Resize { rows, cols }) => {
                        let _ = channel.window_change(cols as u32, rows as u32, 0, 0).await;
                    }
                    Some(SessionCommand::Disconnect) | None => {
                        return DisconnectReason::Requested;
                    }
                    Some(_) => {}
                }
            }
        }
    }
}
