// SPDX-License-Identifier: MIT
//! Hermetic end-to-end verification of `termite-ssh`'s M2 scope.
//!
//! There's no real `sshd` available in CI or this dev environment, so
//! instead of a manual `cargo run` (like M1's terminal was verified) this
//! spins up a real, minimal SSH server in-process using `russh::server` on
//! an ephemeral loopback port, and drives a full connection through it:
//! unknown host key -> approval -> password auth -> shell -> byte echo ->
//! disconnect, then a second connection proving the learned host key is
//! now trusted without re-prompting.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use russh::keys::{Algorithm, PrivateKey};
use russh::server::{self, Auth, Msg, Server as _, Session};
use russh::{Channel, ChannelId, Pty};
use secrecy::SecretString;
use tokio::net::TcpListener;
use tokio::sync::mpsc::Receiver;
use tokio::time::timeout;

use termite_core::{AuthMethod, HostProfile, SessionId};
use termite_ssh::{
    AuthChallenge, AuthResponse, DisconnectReason, SessionCommand, SessionEvent, SshSession,
};

const TEST_USER: &str = "testuser";
const TEST_PASSWORD: &str = "correct horse battery staple";
const RECV_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
struct EchoServer {
    /// If set, `auth_publickey` accepts only this key. Password auth is
    /// unaffected by this field.
    allowed_key: Option<russh::keys::PublicKey>,
}

impl server::Server for EchoServer {
    type Handler = EchoHandler;

    fn new_client(&mut self, _peer_addr: Option<SocketAddr>) -> EchoHandler {
        EchoHandler {
            allowed_key: self.allowed_key.clone(),
        }
    }
}

struct EchoHandler {
    allowed_key: Option<russh::keys::PublicKey>,
}

impl server::Handler for EchoHandler {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if user == TEST_USER && password == TEST_PASSWORD {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        if user == TEST_USER && self.allowed_key.as_ref() == Some(public_key) {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.data(channel, data.to_vec())?;
        Ok(())
    }
}

async fn recv_event(rx: &mut Receiver<(SessionId, SessionEvent)>) -> SessionEvent {
    let (_, event) = timeout(RECV_TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for a session event")
        .expect("session event channel closed unexpectedly");
    event
}

#[tokio::test]
async fn full_session_lifecycle_with_persisted_host_key() {
    let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = socket.local_addr().unwrap().port();

    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let config = Arc::new(server::Config {
        keys: vec![host_key],
        ..Default::default()
    });

    let server_task = tokio::spawn(async move {
        let mut echo_server = EchoServer::default();
        echo_server.run_on_socket(config, &socket).await
    });

    let known_hosts_dir = tempfile::tempdir().unwrap();
    let known_hosts_path = known_hosts_dir.path().join("known_hosts");

    let mut profile = HostProfile::new("test-server", "127.0.0.1", TEST_USER);
    profile.port = port;
    profile.auth = AuthMethod::Password;

    // First connection: the host key is unknown and must be approved
    // before the connection can proceed.
    {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
        let (_session_id, command_tx) =
            SshSession::spawn(profile.clone(), known_hosts_path.clone(), event_tx);

        match recv_event(&mut event_rx).await {
            SessionEvent::HostKeyUnknown(_) => {}
            other => panic!("expected HostKeyUnknown, got {other:?}"),
        }
        command_tx
            .send(SessionCommand::ApproveHostKey(true))
            .await
            .unwrap();

        match recv_event(&mut event_rx).await {
            SessionEvent::AuthRequired(AuthChallenge::Password { .. }) => {}
            other => panic!("expected AuthRequired, got {other:?}"),
        }
        command_tx
            .send(SessionCommand::AuthResponse(AuthResponse::Password(
                SecretString::from(TEST_PASSWORD.to_string()),
            )))
            .await
            .unwrap();

        match recv_event(&mut event_rx).await {
            SessionEvent::Connected => {}
            other => panic!("expected Connected, got {other:?}"),
        }

        command_tx
            .send(SessionCommand::Write(b"hello termite\n".to_vec()))
            .await
            .unwrap();

        match recv_event(&mut event_rx).await {
            SessionEvent::Output(bytes) => assert_eq!(bytes, b"hello termite\n"),
            other => panic!("expected Output, got {other:?}"),
        }

        command_tx.send(SessionCommand::Disconnect).await.unwrap();

        match recv_event(&mut event_rx).await {
            SessionEvent::Disconnected {
                reason: DisconnectReason::Requested,
            } => {}
            other => panic!("expected Disconnected(Requested), got {other:?}"),
        }
    }

    // Second connection, same known_hosts file: the key was learned above,
    // so it must be trusted without another approval prompt.
    {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
        let (_session_id, command_tx) =
            SshSession::spawn(profile.clone(), known_hosts_path.clone(), event_tx);

        match recv_event(&mut event_rx).await {
            SessionEvent::AuthRequired(AuthChallenge::Password { .. }) => {}
            other => {
                panic!("expected AuthRequired directly (host key already trusted), got {other:?}")
            }
        }
        command_tx
            .send(SessionCommand::AuthResponse(AuthResponse::Password(
                SecretString::from(TEST_PASSWORD.to_string()),
            )))
            .await
            .unwrap();

        match recv_event(&mut event_rx).await {
            SessionEvent::Connected => {}
            other => panic!("expected Connected, got {other:?}"),
        }

        command_tx.send(SessionCommand::Disconnect).await.unwrap();
        let _ = recv_event(&mut event_rx).await;
    }

    server_task.abort();
}

#[tokio::test]
async fn publickey_auth_with_unencrypted_key() {
    let _ = env_logger::builder().is_test(true).try_init();
    let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = socket.local_addr().unwrap().port();

    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let client_key = termite_crypto::key::generate_ed25519().unwrap();
    let allowed_key = client_key.public_key().clone();

    let key_dir = tempfile::tempdir().unwrap();
    let key_path = key_dir.path().join("id_ed25519");
    termite_crypto::key::save_to_disk(&client_key, &key_path, None).unwrap();

    let config = Arc::new(server::Config {
        keys: vec![host_key],
        ..Default::default()
    });
    let mut server_task = tokio::spawn(async move {
        let mut echo_server = EchoServer {
            allowed_key: Some(allowed_key),
        };
        echo_server.run_on_socket(config, &socket).await
    });

    let known_hosts_dir = tempfile::tempdir().unwrap();
    let known_hosts_path = known_hosts_dir.path().join("known_hosts");

    let mut profile = HostProfile::new("test-server", "127.0.0.1", TEST_USER);
    profile.port = port;
    profile.auth = AuthMethod::PublicKey { key_path };

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let (_session_id, command_tx) = SshSession::spawn(profile, known_hosts_path, event_tx);

    tokio::select! {
        event = recv_event(&mut event_rx) => {
            match event {
                SessionEvent::HostKeyUnknown(_) => {}
                other => panic!("expected HostKeyUnknown, got {other:?}"),
            }
        }
        result = &mut server_task => {
            panic!("server task ended early: {result:?}");
        }
    }
    command_tx
        .send(SessionCommand::ApproveHostKey(true))
        .await
        .unwrap();

    // An unencrypted key needs no passphrase prompt — straight to Connected.
    match recv_event(&mut event_rx).await {
        SessionEvent::Connected => {}
        other => panic!("expected Connected, got {other:?}"),
    }

    command_tx.send(SessionCommand::Disconnect).await.unwrap();
    let _ = recv_event(&mut event_rx).await;

    server_task.abort();
}

/// Runs a real, in-process SSH agent (russh's agent server) on a temp Unix
/// socket, loads a key into it, points `$SSH_AUTH_SOCK` at it, and drives a
/// full `AuthMethod::Agent` connection. The private key exists only inside
/// the agent task — the session code never sees it.
#[cfg(unix)]
#[tokio::test]
async fn agent_auth_signs_via_the_agent() {
    let _ = env_logger::builder().is_test(true).try_init();

    let agent_dir = tempfile::tempdir().unwrap();
    let agent_path = agent_dir.path().join("agent.sock");
    let listener = tokio::net::UnixListener::bind(&agent_path).unwrap();
    let agent_task = tokio::spawn(russh::keys::agent::server::serve(
        tokio_stream::wrappers::UnixListenerStream::new(listener),
        // `()` implements `Agent` by confirming every request.
        (),
    ));

    let client_key = termite_crypto::key::generate_ed25519().unwrap();
    let allowed_key = client_key.public_key().clone();
    {
        let stream = tokio::net::UnixStream::connect(&agent_path).await.unwrap();
        let mut agent = russh::keys::agent::client::AgentClient::connect(stream);
        agent.add_identity(&client_key, &[]).await.unwrap();
    }
    drop(client_key);

    // Process-global, but this is the only test that reads the variable, so
    // there is nothing to race with (and it hermetically shadows any real
    // agent on the developer's machine).
    std::env::set_var("SSH_AUTH_SOCK", &agent_path);

    let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = socket.local_addr().unwrap().port();
    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let config = Arc::new(server::Config {
        keys: vec![host_key],
        ..Default::default()
    });
    let server_task = tokio::spawn(async move {
        let mut echo_server = EchoServer {
            allowed_key: Some(allowed_key),
        };
        echo_server.run_on_socket(config, &socket).await
    });

    let known_hosts_dir = tempfile::tempdir().unwrap();
    let known_hosts_path = known_hosts_dir.path().join("known_hosts");

    let mut profile = HostProfile::new("test-server", "127.0.0.1", TEST_USER);
    profile.port = port;
    profile.auth = AuthMethod::Agent;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let (_session_id, command_tx) = SshSession::spawn(profile, known_hosts_path, event_tx);

    match recv_event(&mut event_rx).await {
        SessionEvent::HostKeyUnknown(_) => {}
        other => panic!("expected HostKeyUnknown, got {other:?}"),
    }
    command_tx
        .send(SessionCommand::ApproveHostKey(true))
        .await
        .unwrap();

    // Agent auth needs no credential prompts — straight to Connected.
    match recv_event(&mut event_rx).await {
        SessionEvent::Connected => {}
        other => panic!("expected Connected, got {other:?}"),
    }

    command_tx.send(SessionCommand::Disconnect).await.unwrap();
    let _ = recv_event(&mut event_rx).await;

    server_task.abort();
    agent_task.abort();
}

#[tokio::test]
async fn publickey_auth_with_encrypted_key_prompts_for_passphrase() {
    let socket = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = socket.local_addr().unwrap().port();

    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let client_key = termite_crypto::key::generate_ed25519().unwrap();
    let allowed_key = client_key.public_key().clone();
    let passphrase = SecretString::from("correct horse battery staple".to_string());

    let key_dir = tempfile::tempdir().unwrap();
    let key_path = key_dir.path().join("id_ed25519");
    termite_crypto::key::save_to_disk(&client_key, &key_path, Some(&passphrase)).unwrap();

    let config = Arc::new(server::Config {
        keys: vec![host_key],
        ..Default::default()
    });
    let server_task = tokio::spawn(async move {
        let mut echo_server = EchoServer {
            allowed_key: Some(allowed_key),
        };
        echo_server.run_on_socket(config, &socket).await
    });

    let known_hosts_dir = tempfile::tempdir().unwrap();
    let known_hosts_path = known_hosts_dir.path().join("known_hosts");

    let mut profile = HostProfile::new("test-server", "127.0.0.1", TEST_USER);
    profile.port = port;
    profile.auth = AuthMethod::PublicKey { key_path };

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(64);
    let (_session_id, command_tx) = SshSession::spawn(profile, known_hosts_path, event_tx);

    match recv_event(&mut event_rx).await {
        SessionEvent::HostKeyUnknown(_) => {}
        other => panic!("expected HostKeyUnknown, got {other:?}"),
    }
    command_tx
        .send(SessionCommand::ApproveHostKey(true))
        .await
        .unwrap();

    match recv_event(&mut event_rx).await {
        SessionEvent::AuthRequired(AuthChallenge::Passphrase { .. }) => {}
        other => panic!("expected AuthRequired(Passphrase), got {other:?}"),
    }
    command_tx
        .send(SessionCommand::AuthResponse(AuthResponse::Passphrase(
            passphrase,
        )))
        .await
        .unwrap();

    match recv_event(&mut event_rx).await {
        SessionEvent::Connected => {}
        other => panic!("expected Connected, got {other:?}"),
    }

    command_tx.send(SessionCommand::Disconnect).await.unwrap();
    let _ = recv_event(&mut event_rx).await;

    server_task.abort();
}
