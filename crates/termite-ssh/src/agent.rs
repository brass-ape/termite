// SPDX-License-Identifier: MIT
//! Connecting to the user's running SSH agent (`$SSH_AUTH_SOCK` on Unix;
//! the OpenSSH named pipe or Pageant on Windows).

use russh::keys::agent::client::{AgentClient, AgentStream};

/// The boxed-stream form of [`AgentClient`], so Unix sockets, named pipes,
/// and Pageant all present one type to the auth path (and so tests can
/// point `$SSH_AUTH_SOCK` at an in-process agent).
pub(crate) type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

/// Connects to the SSH agent named by `$SSH_AUTH_SOCK`.
#[cfg(unix)]
pub(crate) async fn connect() -> Result<Agent, String> {
    AgentClient::connect_env()
        .await
        .map(AgentClient::dynamic)
        .map_err(|err| format!("cannot connect to the SSH agent ($SSH_AUTH_SOCK): {err}"))
}

/// Connects to OpenSSH-for-Windows's agent pipe, falling back to Pageant.
#[cfg(windows)]
pub(crate) async fn connect() -> Result<Agent, String> {
    // OpenSSH for Windows always serves its agent on this fixed pipe name
    // (there is no $SSH_AUTH_SOCK convention on Windows).
    const OPENSSH_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

    let pipe_err = match AgentClient::connect_named_pipe(OPENSSH_PIPE).await {
        Ok(client) => return Ok(client.dynamic()),
        Err(err) => err,
    };
    AgentClient::connect_pageant()
        .await
        .map(AgentClient::dynamic)
        .map_err(|pageant_err| {
            format!(
                "cannot connect to an SSH agent — OpenSSH pipe: {pipe_err}; \
                 Pageant: {pageant_err}"
            )
        })
}
