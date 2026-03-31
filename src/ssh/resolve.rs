use std::time::Duration;

use crate::{SvnError, SvnUrl};

use super::config::{SshConfig, SshHostKeyPolicy};
use super::openssh::{HostParams, expand_tilde_str, normalize_identity_file_path};

#[derive(Clone, Debug)]
pub(super) struct ResolvedSshSettings {
    pub(super) connect_host: String,
    pub(super) connect_port: u16,
    pub(super) known_hosts_host: String,
    pub(super) username: String,
    pub(super) identity_files: Vec<std::path::PathBuf>,
    pub(super) identity_agent: Option<String>,
    pub(super) identities_only: bool,
    pub(super) host_key: SshHostKeyPolicy,
    pub(super) accept_new_host_keys: bool,
    pub(super) connect_timeout: Duration,
}

fn default_ssh_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("USERNAME").ok())
        .and_then(|username| (!username.trim().is_empty()).then_some(username))
}

fn url_username(url: &SvnUrl) -> Option<String> {
    let rest = url.url.strip_prefix("svn+ssh://")?;
    let authority = rest
        .split_once('/')
        .map(|(authority, _)| authority)
        .unwrap_or(rest);
    let (user, _) = authority.rsplit_once('@')?;
    (!user.trim().is_empty()).then(|| user.to_string())
}

pub(super) fn resolve_ssh_settings(
    url: &SvnUrl,
    ssh: &SshConfig,
    connect_timeout: Duration,
    openssh: Option<&HostParams>,
) -> Result<ResolvedSshSettings, SvnError> {
    let connect_host = openssh
        .and_then(|params| params.host_name.as_ref())
        .cloned()
        .unwrap_or_else(|| url.host.clone());

    let connect_port = if url.port != 22 {
        url.port
    } else {
        openssh.and_then(|params| params.port).unwrap_or(url.port)
    };

    let connect_timeout = openssh
        .and_then(|params| params.connect_timeout)
        .map(|timeout| timeout.min(connect_timeout))
        .unwrap_or(connect_timeout);

    let mut host_key = ssh.host_key.clone();
    let mut accept_new_host_keys = ssh.accept_new_host_keys;

    if matches!(host_key, SshHostKeyPolicy::KnownHosts)
        && let Some(path) = openssh.and_then(|params| params.user_known_hosts_file.as_deref())
    {
        host_key = SshHostKeyPolicy::KnownHostsFile(expand_tilde_str(path));
    }

    if let Some(value) = openssh.and_then(|params| params.strict_host_key_checking.as_deref()) {
        match value.trim().to_ascii_lowercase().as_str() {
            "no" => {
                host_key = SshHostKeyPolicy::AcceptAny;
                accept_new_host_keys = false;
            }
            "accept-new" => accept_new_host_keys = true,
            _ => {}
        }
    }

    let known_hosts_host = openssh
        .and_then(|params| params.host_key_alias.as_ref())
        .cloned()
        .unwrap_or_else(|| url.host.clone());

    let identity_agent = openssh
        .and_then(|params| params.identity_agent.as_ref())
        .cloned();

    let identities_only = openssh
        .and_then(|params| params.identities_only)
        .unwrap_or(false);

    let identity_files = openssh
        .and_then(|params| params.identity_file.as_ref())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|path| normalize_identity_file_path(&path))
        .collect();

    let username = if let Some(username) = ssh.username_override() {
        username.to_string()
    } else if let Some(username) = url_username(url) {
        username
    } else if let Some(username) = openssh.and_then(|params| params.user.as_deref()) {
        username.to_string()
    } else if let Some(username) = default_ssh_username() {
        username
    } else {
        return Err(SvnError::InvalidUrl(
            "svn+ssh requires an SSH username (set SshConfig::with_username, include it in the URL, or configure User in ~/.ssh/config)"
                .to_string(),
        ));
    };

    Ok(ResolvedSshSettings {
        connect_host,
        connect_port,
        known_hosts_host,
        username,
        identity_files,
        identity_agent,
        identities_only,
        host_key,
        accept_new_host_keys,
        connect_timeout,
    })
}
