//! `svn+ssh://` transport support (runs `svnserve -t` over SSH).
//!
//! This module is behind the crate feature `ssh`.

use std::sync::Arc;
use std::time::Duration;

use russh::client;

use crate::{SvnError, SvnUrl};

mod auth;
mod config;
mod openssh;
mod resolve;
#[cfg(test)]
mod tests;

use auth::authenticate_ssh_session;
use openssh::load_openssh_config;
use resolve::{ResolvedSshSettings, resolve_ssh_settings};

pub use config::{SshAuth, SshConfig, SshHostKeyPolicy};

#[derive(Debug)]
pub(super) struct SshClientHandler {
    known_hosts_host: String,
    port: u16,
    host_key: SshHostKeyPolicy,
    accept_new_host_keys: bool,
}

impl client::Handler for SshClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        match &self.host_key {
            SshHostKeyPolicy::AcceptAny => Ok(true),
            SshHostKeyPolicy::KnownHosts => {
                let ok = russh::keys::check_known_hosts(
                    &self.known_hosts_host,
                    self.port,
                    server_public_key,
                )?;
                if ok {
                    return Ok(true);
                }
                if self.accept_new_host_keys {
                    russh::keys::known_hosts::learn_known_hosts(
                        &self.known_hosts_host,
                        self.port,
                        server_public_key,
                    )?;
                    return Ok(true);
                }
                Ok(false)
            }
            SshHostKeyPolicy::KnownHostsFile(path) => {
                let ok = russh::keys::check_known_hosts_path(
                    &self.known_hosts_host,
                    self.port,
                    server_public_key,
                    path,
                )?;
                if ok {
                    return Ok(true);
                }
                if self.accept_new_host_keys {
                    russh::keys::known_hosts::learn_known_hosts_path(
                        &self.known_hosts_host,
                        self.port,
                        server_public_key,
                        path,
                    )?;
                    return Ok(true);
                }
                Ok(false)
            }
        }
    }
}

fn client_handler(settings: &ResolvedSshSettings) -> SshClientHandler {
    SshClientHandler {
        known_hosts_host: settings.known_hosts_host.clone(),
        port: settings.connect_port,
        host_key: settings.host_key.clone(),
        accept_new_host_keys: settings.accept_new_host_keys,
    }
}

pub(crate) async fn open_svnserve_tunnel(
    url: &SvnUrl,
    ssh: &SshConfig,
    connect_timeout: Duration,
) -> Result<russh::ChannelStream<client::Msg>, SvnError> {
    let openssh_params = if ssh.use_openssh_config {
        load_openssh_config().await.map(|cfg| cfg.query(&url.host))
    } else {
        None
    };
    let settings = resolve_ssh_settings(url, ssh, connect_timeout, openssh_params.as_ref())?;

    let config = Arc::new(client::Config::default());
    let handler = client_handler(&settings);

    let connect_fut = async {
        let mut session = client::connect(
            config,
            (&settings.connect_host[..], settings.connect_port),
            handler,
        )
        .await
        .map_err(|err| auth::ssh_io_error("ssh error", err))?;

        if !authenticate_ssh_session(&mut session, ssh, &settings).await? {
            return Err(SvnError::AuthFailed(
                "ssh authentication failed".to_string(),
            ));
        }

        let channel = session
            .channel_open_session()
            .await
            .map_err(|err| auth::ssh_io_error("ssh error", err))?;
        channel
            .exec(true, ssh.command.clone())
            .await
            .map_err(|err| auth::ssh_io_error("ssh error", err))?;
        Ok(channel.into_stream())
    };

    tokio::time::timeout(settings.connect_timeout, connect_fut)
        .await
        .map_err(|_| {
            SvnError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "ssh connect timed out",
            ))
        })?
}
