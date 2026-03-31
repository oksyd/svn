use std::fmt::Display;
use std::path::Path;
use std::sync::Arc;

use russh::client;
use russh::keys::PrivateKeyWithHashAlg;
use tracing::debug;

use crate::SvnError;

use super::SshClientHandler;
use super::config::{SshAuth, SshConfig};
use super::openssh::{default_identity_files, expand_tilde_path};
use super::resolve::ResolvedSshSettings;

type DynAgent = russh::keys::agent::client::AgentClient<
    Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin + 'static>,
>;
type SshSession = client::Handle<SshClientHandler>;

pub(super) fn ssh_io_error(label: &str, err: impl Display) -> SvnError {
    SvnError::Io(std::io::Error::other(format!("{label}: {err}")))
}

#[cfg(unix)]
async fn connect_agent(identity_agent: Option<&str>) -> Option<DynAgent> {
    let mut requested = identity_agent
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if matches!(requested, Some("none")) {
        return None;
    }
    if matches!(requested, Some("SSH_AUTH_SOCK")) {
        requested = None;
    }

    let client = if let Some(path) = requested {
        russh::keys::agent::client::AgentClient::connect_uds(path).await
    } else {
        russh::keys::agent::client::AgentClient::connect_env().await
    };

    match client {
        Ok(client) => Some(client.dynamic()),
        Err(err) => {
            debug!(error = %err, "ssh-agent unavailable");
            None
        }
    }
}

#[cfg(windows)]
async fn connect_named_pipe_dyn(path: &str) -> Result<DynAgent, russh::keys::Error> {
    russh::keys::agent::client::AgentClient::connect_named_pipe(path)
        .await
        .map(|client| client.dynamic())
}

#[cfg(windows)]
async fn connect_agent(identity_agent: Option<&str>) -> Option<DynAgent> {
    let mut requested = identity_agent
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if matches!(requested, Some("none")) {
        return None;
    }
    if matches!(requested, Some("SSH_AUTH_SOCK")) {
        requested = None;
    }

    if let Some(path) = requested {
        match connect_named_pipe_dyn(path).await {
            Ok(client) => return Some(client),
            Err(err) => debug!(error = %err, "ssh-agent named pipe unavailable"),
        }
    }

    if let Ok(sock) = std::env::var("SSH_AUTH_SOCK")
        && !sock.trim().is_empty()
    {
        match connect_named_pipe_dyn(sock.trim()).await {
            Ok(client) => return Some(client),
            Err(err) => debug!(error = %err, "ssh-agent SSH_AUTH_SOCK unavailable"),
        }
    }

    match connect_named_pipe_dyn(r"\\.\pipe\openssh-ssh-agent").await {
        Ok(client) => Some(client),
        Err(err) => {
            debug!(error = %err, "OpenSSH agent named pipe unavailable");
            match russh::keys::agent::client::AgentClient::connect_pageant().await {
                Ok(client) => Some(client.dynamic()),
                Err(err) => {
                    debug!(error = %err, "Pageant agent unavailable");
                    None
                }
            }
        }
    }
}

struct CloningAgentSigner {
    agent: DynAgent,
}

impl russh::Signer for CloningAgentSigner {
    type Error = russh::AgentAuthError;

    #[allow(clippy::manual_async_fn)]
    fn auth_sign(
        &mut self,
        key: &russh::keys::agent::AgentIdentity,
        hash_alg: Option<russh::keys::HashAlg>,
        to_sign: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        let agent = &mut self.agent;
        let key = key.clone();
        async move {
            agent
                .sign_request(&key, hash_alg, to_sign)
                .await
                .map_err(Into::into)
        }
    }
}

async fn try_authenticate_with_agent(
    session: &mut SshSession,
    username: &str,
    identity_agent: Option<&str>,
) -> Result<bool, SvnError> {
    let Some(mut agent) = connect_agent(identity_agent).await else {
        return Ok(false);
    };

    let keys = agent
        .request_identities()
        .await
        .map_err(|err| ssh_io_error("ssh-agent error", err))?;
    if keys.is_empty() {
        return Ok(false);
    }

    let mut signer = CloningAgentSigner { agent };

    let hash_alg = session
        .best_supported_rsa_hash()
        .await
        .map_err(|err| ssh_io_error("ssh error", err))?
        .flatten();

    for key in keys {
        let result = match &key {
            russh::keys::agent::AgentIdentity::PublicKey {
                key: public_key, ..
            } => {
                session
                    .authenticate_publickey_with(
                        username.to_string(),
                        public_key.clone(),
                        hash_alg,
                        &mut signer,
                    )
                    .await
            }
            russh::keys::agent::AgentIdentity::Certificate { certificate, .. } => {
                session
                    .authenticate_certificate_with(
                        username.to_string(),
                        certificate.clone(),
                        hash_alg,
                        &mut signer,
                    )
                    .await
            }
        }
        .map_err(|err| ssh_io_error("ssh-agent error", err))?;
        if result.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn try_authenticate_with_keyfile(
    session: &mut SshSession,
    username: &str,
    path: &Path,
    passphrase: Option<&str>,
    strict: bool,
) -> Result<bool, SvnError> {
    let key_pair = match russh::keys::load_secret_key(path, passphrase) {
        Ok(key_pair) => key_pair,
        Err(err) if !strict => {
            debug!(path = %path.display(), error = %err, "failed to load identity; skipping");
            return Ok(false);
        }
        Err(err) => return Err(ssh_io_error("ssh key error", err)),
    };
    let key_pair = Arc::new(key_pair);

    let hash_alg = session
        .best_supported_rsa_hash()
        .await
        .map_err(|err| ssh_io_error("ssh error", err))?
        .flatten();

    let result = session
        .authenticate_publickey(
            username.to_string(),
            PrivateKeyWithHashAlg::new(key_pair, hash_alg),
        )
        .await
        .map_err(|err| ssh_io_error("ssh error", err))?;
    Ok(result.success())
}

pub(super) async fn authenticate_ssh_session(
    session: &mut SshSession,
    ssh: &SshConfig,
    settings: &ResolvedSshSettings,
) -> Result<bool, SvnError> {
    let username = settings.username.as_str();

    if ssh.try_ssh_agent
        && !settings.identities_only
        && try_authenticate_with_agent(session, username, settings.identity_agent.as_deref())
            .await?
    {
        return Ok(true);
    }

    if let SshAuth::Password(password) = &ssh.auth {
        let result = session
            .authenticate_password(username.to_string(), password.clone())
            .await
            .map_err(|err| ssh_io_error("ssh error", err))?;
        return Ok(result.success());
    }

    if let SshAuth::KeyFile { path, passphrase } = &ssh.auth {
        let path = expand_tilde_path(path);
        return try_authenticate_with_keyfile(
            session,
            username,
            &path,
            passphrase.as_deref(),
            true,
        )
        .await;
    }

    if ssh.try_default_identities {
        for identity in &settings.identity_files {
            if try_authenticate_with_keyfile(session, username, identity, None, false).await? {
                return Ok(true);
            }
        }

        for identity in default_identity_files() {
            if try_authenticate_with_keyfile(session, username, &identity, None, false).await? {
                return Ok(true);
            }
        }
    }

    let result = session
        .authenticate_none(username.to_string())
        .await
        .map_err(|err| ssh_io_error("ssh error", err))?;
    Ok(result.success())
}
