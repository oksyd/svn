use std::path::PathBuf;

/// SSH authentication options for `svn+ssh://` transports.
#[derive(Clone, Debug)]
pub enum SshAuth {
    /// Password authentication.
    Password(String),
    /// Public key authentication from an OpenSSH private key file.
    KeyFile {
        /// Path to a private key (for example `~/.ssh/id_ed25519`).
        ///
        /// `~` is expanded to the current user's home directory.
        path: PathBuf,
        /// Optional passphrase for an encrypted private key.
        passphrase: Option<String>,
    },
    /// Use the SSH "none" authentication method.
    ///
    /// This only attempts the SSH "none" method. If you want automatic
    /// authentication via ssh-agent and/or default key files, use
    /// [`SshConfig::default`], [`SshConfig::with_ssh_agent`], and/or
    /// [`SshConfig::with_default_identities`].
    None,
}

/// SSH host key verification policy.
#[derive(Clone, Debug)]
pub enum SshHostKeyPolicy {
    /// Accept any server host key (insecure; vulnerable to MITM).
    AcceptAny,
    /// Verify the server host key against the user's `~/.ssh/known_hosts`.
    KnownHosts,
    /// Verify the server host key against the given `known_hosts` file.
    KnownHostsFile(PathBuf),
}

/// Configuration for the `svn+ssh://` transport.
#[derive(Clone, Debug)]
pub struct SshConfig {
    username: Option<String>,
    pub(super) auth: SshAuth,
    pub(super) host_key: SshHostKeyPolicy,
    pub(super) command: String,
    pub(super) use_openssh_config: bool,
    pub(super) try_ssh_agent: bool,
    pub(super) try_default_identities: bool,
    pub(super) accept_new_host_keys: bool,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            username: None,
            auth: SshAuth::None,
            host_key: SshHostKeyPolicy::KnownHosts,
            command: "svnserve -t".to_string(),
            use_openssh_config: true,
            try_ssh_agent: true,
            try_default_identities: true,
            accept_new_host_keys: false,
        }
    }
}

impl SshConfig {
    /// Creates an SSH config for `svn+ssh://`.
    ///
    /// By default this:
    /// - verifies the server host key against `~/.ssh/known_hosts`;
    /// - reads `~/.ssh/config`;
    /// - runs `svnserve -t`.
    pub fn new(auth: SshAuth) -> Self {
        Self {
            username: None,
            auth,
            host_key: SshHostKeyPolicy::KnownHosts,
            command: "svnserve -t".to_string(),
            use_openssh_config: true,
            try_ssh_agent: false,
            try_default_identities: false,
            accept_new_host_keys: false,
        }
    }

    /// Sets the SSH username (overrides any username embedded in the URL).
    #[must_use]
    pub fn with_username(mut self, username: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self
    }

    /// Disables host key verification (insecure).
    #[must_use]
    pub fn accept_any_host_key(mut self) -> Self {
        self.host_key = SshHostKeyPolicy::AcceptAny;
        self
    }

    /// Uses a custom `known_hosts` file for host key verification.
    #[must_use]
    pub fn with_known_hosts_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.host_key = SshHostKeyPolicy::KnownHostsFile(path.into());
        self
    }

    /// Sets the remote command executed over SSH (default: `svnserve -t`).
    #[must_use]
    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = command.into();
        self
    }

    /// Enables or disables reading OpenSSH configuration (`~/.ssh/config`).
    ///
    /// When enabled, `HostName`, `Port`, `User`, and `IdentityFile` entries can
    /// influence how the SSH connection is established.
    #[must_use]
    pub fn with_openssh_config(mut self, enabled: bool) -> Self {
        self.use_openssh_config = enabled;
        self
    }

    /// Attempts public key authentication via the local SSH agent (for example,
    /// `SSH_AUTH_SOCK` on Unix, OpenSSH agent / Pageant on Windows).
    #[must_use]
    pub fn with_ssh_agent(mut self) -> Self {
        self.try_ssh_agent = true;
        self
    }

    /// Attempts a set of default key files (for example `~/.ssh/id_ed25519`).
    #[must_use]
    pub fn with_default_identities(mut self) -> Self {
        self.try_default_identities = true;
        self
    }

    /// Accepts and records new host keys into the `known_hosts` file when the
    /// host is not found.
    ///
    /// This does **not** ignore host key changes (changed keys are always
    /// rejected).
    #[must_use]
    pub fn accept_new_host_keys(mut self) -> Self {
        self.accept_new_host_keys = true;
        self
    }

    pub(super) fn username_override(&self) -> Option<&str> {
        self.username.as_deref()
    }
}
