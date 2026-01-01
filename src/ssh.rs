//! `svn+ssh://` transport support (runs `svnserve -t` over SSH).
//!
//! This module is behind the crate feature `ssh`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use russh::client;
use russh::keys::PrivateKeyWithHashAlg;
use tokio::sync::OnceCell;
use tracing::debug;

use crate::{SvnError, SvnUrl};

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
    pub(crate) auth: SshAuth,
    pub(crate) host_key: SshHostKeyPolicy,
    pub(crate) command: String,
    pub(crate) use_openssh_config: bool,
    pub(crate) try_ssh_agent: bool,
    pub(crate) try_default_identities: bool,
    pub(crate) accept_new_host_keys: bool,
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

    pub(crate) fn username_override(&self) -> Option<&str> {
        self.username.as_deref()
    }
}

#[derive(Debug)]
struct SshClientHandler {
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

fn default_ssh_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("USERNAME").ok())
        .and_then(|u| (!u.trim().is_empty()).then_some(u))
}

fn url_username(url: &SvnUrl) -> Option<String> {
    let rest = url.url.strip_prefix("svn+ssh://")?;
    let authority = rest.split_once('/').map(|(a, _)| a).unwrap_or(rest);
    let (user, _) = authority.rsplit_once('@')?;
    (!user.trim().is_empty()).then(|| user.to_string())
}

#[derive(Clone, Debug, Default)]
struct HostParams {
    host_name: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    identity_file: Option<Vec<PathBuf>>,
    connect_timeout: Option<Duration>,
    host_key_alias: Option<String>,
    user_known_hosts_file: Option<String>,
    strict_host_key_checking: Option<String>,
    identity_agent: Option<String>,
    identities_only: Option<bool>,
}

#[derive(Clone, Debug)]
struct HostPattern {
    negated: bool,
    pattern: String,
}

#[derive(Clone, Debug, Default)]
struct HostBlock {
    patterns: Vec<HostPattern>,
    params: HostParams,
}

impl HostBlock {
    fn matches_host(&self, host: &str) -> bool {
        if self.patterns.is_empty() {
            return true;
        }

        let host = host.to_ascii_lowercase();
        let mut matched_positive = false;

        for pat in &self.patterns {
            if wildcard_match(pat.pattern.as_str(), host.as_str()) {
                if pat.negated {
                    return false;
                }
                matched_positive = true;
            }
        }

        matched_positive
    }
}

#[derive(Clone, Debug, Default)]
struct OpenSshConfig {
    blocks: Vec<HostBlock>,
}

impl OpenSshConfig {
    fn parse_default_file() -> Result<Option<Self>, std::io::Error> {
        let Some(home) = home_dir() else {
            return Ok(None);
        };
        let path = home.join(".ssh").join("config");
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };

        let mut parser = OpenSshParser::new();
        let mut include_stack = Vec::new();
        parser.parse_str(
            &String::from_utf8_lossy(&bytes),
            path.parent(),
            &mut include_stack,
            0,
        )?;
        Ok(Some(Self {
            blocks: parser.blocks,
        }))
    }

    #[cfg(test)]
    fn parse_str(input: &str) -> Self {
        let mut parser = OpenSshParser::new();
        let mut include_stack = Vec::new();
        let _ = parser.parse_str(input, None, &mut include_stack, 0);
        Self {
            blocks: parser.blocks,
        }
    }

    fn query(&self, host: &str) -> HostParams {
        let mut out = HostParams::default();

        for block in &self.blocks {
            if !block.matches_host(host) {
                continue;
            }

            if out.host_name.is_none() {
                out.host_name = block.params.host_name.clone();
            }
            if out.port.is_none() {
                out.port = block.params.port;
            }
            if out.user.is_none() {
                out.user = block.params.user.clone();
            }
            if out.connect_timeout.is_none() {
                out.connect_timeout = block.params.connect_timeout;
            }
            if out.host_key_alias.is_none() {
                out.host_key_alias = block.params.host_key_alias.clone();
            }
            if out.user_known_hosts_file.is_none() {
                out.user_known_hosts_file = block.params.user_known_hosts_file.clone();
            }
            if out.strict_host_key_checking.is_none() {
                out.strict_host_key_checking = block.params.strict_host_key_checking.clone();
            }
            if out.identity_agent.is_none() {
                out.identity_agent = block.params.identity_agent.clone();
            }
            if out.identities_only.is_none() {
                out.identities_only = block.params.identities_only;
            }
            if let Some(files) = &block.params.identity_file {
                out.identity_file
                    .get_or_insert_with(Vec::new)
                    .extend(files.iter().cloned());
            }
        }

        out
    }
}

static OPENSSH_CONFIG: OnceCell<Option<OpenSshConfig>> = OnceCell::const_new();

async fn load_openssh_config() -> Option<&'static OpenSshConfig> {
    let config = OPENSSH_CONFIG
        .get_or_init(|| async {
            match tokio::task::spawn_blocking(OpenSshConfig::parse_default_file).await {
                Ok(Ok(Some(cfg))) => Some(cfg),
                Ok(Ok(None)) => None,
                Ok(Err(err)) => {
                    debug!(error = %err, "failed to read ~/.ssh/config; ignoring");
                    None
                }
                Err(err) => {
                    debug!(error = %err, "failed to join ~/.ssh/config parse task; ignoring");
                    None
                }
            }
        })
        .await;
    config.as_ref()
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" | "on" => Some(true),
        "no" | "false" | "0" | "off" => Some(false),
        _ => None,
    }
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let text = text.to_ascii_lowercase();
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();

    let mut p = 0usize;
    let mut t = 0usize;
    let mut star = None::<usize>;
    let mut star_match = 0usize;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
            continue;
        }
        if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_match = t;
            continue;
        }
        if let Some(star_idx) = star {
            p = star_idx + 1;
            star_match += 1;
            t = star_match;
            continue;
        }
        return false;
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

struct OpenSshParser {
    blocks: Vec<HostBlock>,
    current: usize,
    skip_match_block: bool,
}

impl OpenSshParser {
    fn new() -> Self {
        Self {
            blocks: vec![HostBlock::default()],
            current: 0,
            skip_match_block: false,
        }
    }

    fn parse_str(
        &mut self,
        input: &str,
        base_dir: Option<&Path>,
        include_stack: &mut Vec<PathBuf>,
        include_depth: usize,
    ) -> Result<(), std::io::Error> {
        let mut continuation = String::new();

        for raw in input.lines() {
            let raw = raw.trim_end_matches('\r');

            if continuation.is_empty() {
                continuation.push_str(raw);
            } else {
                continuation.push(' ');
                continuation.push_str(raw.trim_start());
            }

            if is_line_continued(continuation.as_str()) {
                continuation.pop();
                continue;
            }

            let parsed = parse_config_line_tokens(continuation.trim());
            continuation.clear();

            let Some((key, values)) = parsed else {
                continue;
            };

            let key = key.to_ascii_lowercase();

            if self.skip_match_block && key != "host" && key != "match" {
                continue;
            }
            if self.skip_match_block && (key == "host" || key == "match") {
                self.skip_match_block = false;
            }

            match key.as_str() {
                "host" => {
                    let patterns = parse_host_patterns(&values);
                    if patterns.is_empty() {
                        continue;
                    }
                    self.blocks.push(HostBlock {
                        patterns,
                        params: HostParams::default(),
                    });
                    self.current = self.blocks.len() - 1;
                }
                "match" => {
                    if let Some(patterns) = parse_match_as_host_patterns(&values) {
                        self.blocks.push(HostBlock {
                            patterns,
                            params: HostParams::default(),
                        });
                        self.current = self.blocks.len() - 1;
                    } else {
                        self.skip_match_block = true;
                    }
                }
                "include" => {
                    for pattern in &values {
                        self.parse_include_pattern(
                            pattern,
                            base_dir,
                            include_stack,
                            include_depth,
                        )?;
                    }
                }
                _ => self.apply_option(key.as_str(), &values),
            }
        }

        Ok(())
    }

    fn parse_include_pattern(
        &mut self,
        pattern: &str,
        base_dir: Option<&Path>,
        include_stack: &mut Vec<PathBuf>,
        include_depth: usize,
    ) -> Result<(), std::io::Error> {
        const MAX_INCLUDE_DEPTH: usize = 16;
        if include_depth >= MAX_INCLUDE_DEPTH {
            return Ok(());
        }

        let mut path = expand_tilde_str(pattern);
        if path.is_relative()
            && let Some(dir) = base_dir
        {
            path = dir.join(path);
        }

        let include_paths = match expand_include_paths(&path) {
            Ok(p) => p,
            Err(_) => return Ok(()),
        };
        for include_path in include_paths {
            self.parse_file(&include_path, include_stack, include_depth + 1);
        }
        Ok(())
    }

    fn parse_file(&mut self, path: &Path, include_stack: &mut Vec<PathBuf>, depth: usize) {
        let path = match std::fs::canonicalize(path) {
            Ok(p) => p,
            Err(_) => path.to_path_buf(),
        };
        if include_stack.contains(&path) {
            return;
        }
        include_stack.push(path.clone());

        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => {
                include_stack.pop();
                return;
            }
        };
        let _ = self.parse_str(
            &String::from_utf8_lossy(&bytes),
            path.parent(),
            include_stack,
            depth,
        );
        include_stack.pop();
    }

    fn apply_option(&mut self, key: &str, values: &[String]) {
        let params = &mut self.blocks[self.current].params;

        match key {
            "hostname" => {
                if params.host_name.is_none()
                    && let Some(v) = values.first()
                {
                    params.host_name = Some(v.to_string());
                }
            }
            "user" => {
                if params.user.is_none()
                    && let Some(v) = values.first()
                {
                    params.user = Some(v.to_string());
                }
            }
            "port" => {
                if params.port.is_none()
                    && let Some(v) = values.first()
                    && let Ok(p) = v.parse::<u16>()
                {
                    params.port = Some(p);
                }
            }
            "identityfile" => {
                if let Some(v) = values.first() {
                    params
                        .identity_file
                        .get_or_insert_with(Vec::new)
                        .push(PathBuf::from(v));
                }
            }
            "identityagent" => {
                if params.identity_agent.is_none()
                    && let Some(v) = values.first()
                {
                    params.identity_agent = Some(v.to_string());
                }
            }
            "identitiesonly" => {
                if params.identities_only.is_none()
                    && let Some(v) = values.first()
                    && let Some(b) = parse_bool(v)
                {
                    params.identities_only = Some(b);
                }
            }
            "hostkeyalias" => {
                if params.host_key_alias.is_none()
                    && let Some(v) = values.first()
                {
                    params.host_key_alias = Some(v.to_string());
                }
            }
            "userknownhostsfile" => {
                if params.user_known_hosts_file.is_none()
                    && let Some(v) = values.first()
                {
                    params.user_known_hosts_file = Some(v.to_string());
                }
            }
            "stricthostkeychecking" => {
                if params.strict_host_key_checking.is_none()
                    && let Some(v) = values.first()
                {
                    params.strict_host_key_checking = Some(v.to_string());
                }
            }
            "connecttimeout" => {
                if params.connect_timeout.is_none()
                    && let Some(v) = values.first()
                    && let Ok(secs) = v.parse::<u64>()
                {
                    params.connect_timeout = Some(Duration::from_secs(secs));
                }
            }
            _ => {}
        }
    }
}

fn is_line_continued(line: &str) -> bool {
    let line = line.trim_end();
    line.ends_with('\\') && !line.ends_with("\\\\")
}

fn parse_config_line_tokens(line: &str) -> Option<(String, Vec<String>)> {
    let mut tokens = tokenize_ssh_config_line(line);
    if tokens.is_empty() {
        return None;
    }

    if tokens.len() >= 3 && tokens[1] == "=" {
        let key = tokens.remove(0);
        tokens.remove(0);
        return Some((key, tokens));
    }

    if let Some((k, v)) = tokens[0].split_once('=')
        && !k.is_empty()
    {
        let key = k.to_string();
        let mut values = Vec::new();
        if !v.is_empty() {
            values.push(v.to_string());
        }
        values.extend(tokens.into_iter().skip(1));
        return Some((key, values));
    }

    let key = tokens.remove(0);
    Some((key, tokens))
}

fn tokenize_ssh_config_line(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '#' && !in_single && !in_double {
            break;
        }

        if c == '\\' && !in_single {
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }

        if c == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            continue;
        }

        if c.is_whitespace() && !in_single && !in_double {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }

        current.push(c);
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn parse_host_patterns(values: &[String]) -> Vec<HostPattern> {
    values
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| {
            let (negated, pattern) = v.strip_prefix('!').map(|p| (true, p)).unwrap_or((false, v));
            HostPattern {
                negated,
                pattern: pattern.to_ascii_lowercase(),
            }
        })
        .collect()
}

fn is_match_criterion_keyword(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "all"
            | "canonical"
            | "exec"
            | "final"
            | "host"
            | "originalhost"
            | "user"
            | "localuser"
            | "localnetwork"
            | "tagged"
            | "address"
            | "rdomain"
    )
}

fn parse_match_as_host_patterns(values: &[String]) -> Option<Vec<HostPattern>> {
    let mut iter = values.iter().map(|v| v.as_str());
    let first = iter.next()?.to_ascii_lowercase();

    if first == "all" && values.len() == 1 {
        return Some(vec![HostPattern {
            negated: false,
            pattern: "*".to_string(),
        }]);
    }

    if first != "host" && first != "originalhost" {
        return None;
    }

    if values.iter().skip(1).any(|t| is_match_criterion_keyword(t)) {
        return None;
    }

    let patterns = parse_host_patterns(&values[1..]);
    (!patterns.is_empty()).then_some(patterns)
}

fn expand_include_paths(path: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let s = path.to_string_lossy();
    if !s.contains('*') && !s.contains('?') && !s.contains('[') {
        return Ok(vec![path.to_path_buf()]);
    }

    let Some(parent) = path.parent() else {
        return Ok(Vec::new());
    };
    let Some(file_pat) = path.file_name().and_then(|n| n.to_str()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    let entries = match std::fs::read_dir(parent) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if wildcard_match(file_pat, name) {
            out.push(entry.path());
        }
    }

    out.sort();
    Ok(out)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            if drive.is_empty() || path.is_empty() {
                None
            } else {
                Some(PathBuf::from(drive).join(path))
            }
        })
}

fn expand_tilde_str(s: &str) -> PathBuf {
    if s == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\"))
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(s)
}

fn expand_tilde_path(path: &Path) -> PathBuf {
    let mut components = path.components();
    let Some(first) = components.next() else {
        return path.to_path_buf();
    };
    if first.as_os_str() != OsStr::new("~") {
        return path.to_path_buf();
    }
    let Some(home) = home_dir() else {
        return path.to_path_buf();
    };
    let mut out = home;
    out.extend(components);
    out
}

fn normalize_identity_file_path(path: &Path) -> PathBuf {
    let path = expand_tilde_path(path);
    if path.is_relative()
        && let Some(home) = home_dir()
    {
        return home.join(path);
    }
    path
}

fn default_identity_files() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let ssh = home.join(".ssh");
    [
        "id_ed25519",
        "id_ed25519_sk",
        "id_ecdsa",
        "id_ecdsa_sk",
        "id_rsa",
        "id_dsa",
    ]
    .into_iter()
    .map(|name| ssh.join(name))
    .collect()
}

#[derive(Clone, Debug)]
struct ResolvedSshSettings {
    connect_host: String,
    connect_port: u16,
    known_hosts_host: String,
    username: String,
    identity_files: Vec<PathBuf>,
    identity_agent: Option<String>,
    identities_only: bool,
    host_key: SshHostKeyPolicy,
    accept_new_host_keys: bool,
    connect_timeout: Duration,
}

fn resolve_ssh_settings(
    url: &SvnUrl,
    ssh: &SshConfig,
    connect_timeout: Duration,
    openssh: Option<&HostParams>,
) -> Result<ResolvedSshSettings, SvnError> {
    let connect_host = openssh
        .and_then(|p| p.host_name.as_ref())
        .cloned()
        .unwrap_or_else(|| url.host.clone());

    let connect_port = if url.port != 22 {
        url.port
    } else {
        openssh.and_then(|p| p.port).unwrap_or(url.port)
    };

    let connect_timeout = openssh
        .and_then(|p| p.connect_timeout)
        .map(|t| t.min(connect_timeout))
        .unwrap_or(connect_timeout);

    let mut host_key = ssh.host_key.clone();
    let mut accept_new_host_keys = ssh.accept_new_host_keys;

    if matches!(host_key, SshHostKeyPolicy::KnownHosts)
        && let Some(path) = openssh.and_then(|p| p.user_known_hosts_file.as_deref())
    {
        host_key = SshHostKeyPolicy::KnownHostsFile(expand_tilde_str(path));
    }

    if let Some(value) = openssh.and_then(|p| p.strict_host_key_checking.as_deref()) {
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
        .and_then(|p| p.host_key_alias.as_ref())
        .cloned()
        .unwrap_or_else(|| url.host.clone());

    let identity_agent = openssh.and_then(|p| p.identity_agent.as_ref()).cloned();

    let identities_only = openssh.and_then(|p| p.identities_only).unwrap_or(false);

    let mut identity_files = openssh
        .and_then(|p| p.identity_file.as_ref())
        .cloned()
        .unwrap_or_default();
    identity_files = identity_files
        .into_iter()
        .map(|p| normalize_identity_file_path(&p))
        .collect();

    let username = if let Some(u) = ssh.username_override() {
        u.to_string()
    } else if let Some(u) = url_username(url) {
        u
    } else if let Some(u) = openssh.and_then(|p| p.user.as_deref()) {
        u.to_string()
    } else if let Some(u) = default_ssh_username() {
        u
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

type DynAgent = russh::keys::agent::client::AgentClient<
    Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin + 'static>,
>;

#[cfg(unix)]
async fn connect_agent(identity_agent: Option<&str>) -> Option<DynAgent> {
    let mut requested = identity_agent.map(str::trim).filter(|v| !v.is_empty());
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
        Ok(c) => Some(c.dynamic()),
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
        .map(|c| c.dynamic())
}

#[cfg(windows)]
async fn connect_agent(identity_agent: Option<&str>) -> Option<DynAgent> {
    let mut requested = identity_agent.map(str::trim).filter(|v| !v.is_empty());
    if matches!(requested, Some("none")) {
        return None;
    }
    if matches!(requested, Some("SSH_AUTH_SOCK")) {
        requested = None;
    }

    if let Some(path) = requested {
        match connect_named_pipe_dyn(path).await {
            Ok(c) => return Some(c),
            Err(err) => debug!(error = %err, "ssh-agent named pipe unavailable"),
        }
    }

    if let Ok(sock) = std::env::var("SSH_AUTH_SOCK")
        && !sock.trim().is_empty()
    {
        match connect_named_pipe_dyn(sock.trim()).await {
            Ok(c) => return Some(c),
            Err(err) => debug!(error = %err, "ssh-agent SSH_AUTH_SOCK unavailable"),
        }
    }

    match connect_named_pipe_dyn(r"\\.\pipe\openssh-ssh-agent").await {
        Ok(c) => Some(c),
        Err(err) => {
            debug!(error = %err, "OpenSSH agent named pipe unavailable");
            match russh::keys::agent::client::AgentClient::connect_pageant().await {
                Ok(c) => Some(c.dynamic()),
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
    fn auth_publickey_sign(
        &mut self,
        key: &russh::keys::PublicKey,
        hash_alg: Option<russh::keys::HashAlg>,
        to_sign: russh::CryptoVec,
    ) -> impl std::future::Future<Output = Result<russh::CryptoVec, Self::Error>> + Send {
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
    session: &mut client::Handle<SshClientHandler>,
    username: &str,
    identity_agent: Option<&str>,
) -> Result<bool, SvnError> {
    let Some(mut agent) = connect_agent(identity_agent).await else {
        return Ok(false);
    };

    let keys = agent
        .request_identities()
        .await
        .map_err(|e| SvnError::Io(std::io::Error::other(format!("ssh-agent error: {e}"))))?;
    if keys.is_empty() {
        return Ok(false);
    }

    let mut signer = CloningAgentSigner { agent };

    let hash_alg = session
        .best_supported_rsa_hash()
        .await
        .map_err(|e| SvnError::Io(std::io::Error::other(format!("ssh error: {e}"))))?
        .flatten();

    for key in keys {
        let result = session
            .authenticate_publickey_with(username.to_string(), key, hash_alg, &mut signer)
            .await
            .map_err(|e| SvnError::Io(std::io::Error::other(format!("ssh-agent error: {e}"))))?;
        if result.success() {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn try_authenticate_with_keyfile(
    session: &mut client::Handle<SshClientHandler>,
    username: &str,
    path: &Path,
    passphrase: Option<&str>,
    strict: bool,
) -> Result<bool, SvnError> {
    let key_pair = match russh::keys::load_secret_key(path, passphrase) {
        Ok(k) => k,
        Err(err) if !strict => {
            debug!(path = %path.display(), error = %err, "failed to load identity; skipping");
            return Ok(false);
        }
        Err(err) => {
            return Err(SvnError::Io(std::io::Error::other(format!(
                "ssh key error: {err}"
            ))));
        }
    };
    let key_pair = Arc::new(key_pair);

    let hash_alg = session
        .best_supported_rsa_hash()
        .await
        .map_err(|e| SvnError::Io(std::io::Error::other(format!("ssh error: {e}"))))?
        .flatten();

    let result = session
        .authenticate_publickey(
            username.to_string(),
            PrivateKeyWithHashAlg::new(key_pair, hash_alg),
        )
        .await
        .map_err(|e| SvnError::Io(std::io::Error::other(format!("ssh error: {e}"))))?;
    Ok(result.success())
}

async fn authenticate_ssh_session(
    session: &mut client::Handle<SshClientHandler>,
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
            .map_err(|e| SvnError::Io(std::io::Error::other(format!("ssh error: {e}"))))?;
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
        .map_err(|e| SvnError::Io(std::io::Error::other(format!("ssh error: {e}"))))?;
    Ok(result.success())
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
    let handler = SshClientHandler {
        known_hosts_host: settings.known_hosts_host.clone(),
        port: settings.connect_port,
        host_key: settings.host_key.clone(),
        accept_new_host_keys: settings.accept_new_host_keys,
    };

    let connect_fut = async {
        let mut session = client::connect(
            config,
            (&settings.connect_host[..], settings.connect_port),
            handler,
        )
        .await
        .map_err(|e| SvnError::Io(std::io::Error::other(format!("ssh error: {e}"))))?;

        if !authenticate_ssh_session(&mut session, ssh, &settings).await? {
            return Err(SvnError::AuthFailed(
                "ssh authentication failed".to_string(),
            ));
        }

        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| SvnError::Io(std::io::Error::other(format!("ssh error: {e}"))))?;
        channel
            .exec(true, ssh.command.clone())
            .await
            .map_err(|e| SvnError::Io(std::io::Error::other(format!("ssh error: {e}"))))?;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn resolve_ssh_username_prefers_override() {
        let url = SvnUrl::parse("svn+ssh://alice@example.com/repo").unwrap();
        let ssh = SshConfig::new(SshAuth::None).with_username("bob");
        let settings = resolve_ssh_settings(&url, &ssh, Duration::from_secs(1), None).unwrap();
        assert_eq!(settings.username, "bob");
    }

    #[test]
    fn resolve_ssh_username_uses_url_user() {
        let url = SvnUrl::parse("svn+ssh://alice@example.com/repo").unwrap();
        let ssh = SshConfig::new(SshAuth::None);
        let settings = resolve_ssh_settings(&url, &ssh, Duration::from_secs(1), None).unwrap();
        assert_eq!(settings.username, "alice");
    }

    #[test]
    fn openssh_config_can_override_host_user_and_port() {
        let cfg = OpenSshConfig::parse_str(
            "Host example.com\n  HostName real.example.com\n  User alice\n  Port 2222\n",
        );
        let params = cfg.query("example.com");

        let url = SvnUrl::parse("svn+ssh://example.com/repo").unwrap();
        let ssh = SshConfig::new(SshAuth::None);
        let settings =
            resolve_ssh_settings(&url, &ssh, Duration::from_secs(30), Some(&params)).unwrap();
        assert_eq!(settings.connect_host, "real.example.com");
        assert_eq!(settings.connect_port, 2222);
        assert_eq!(settings.username, "alice");
    }

    #[test]
    fn openssh_config_does_not_override_explicit_url_port() {
        let cfg = OpenSshConfig::parse_str("Host example.com\n  Port 2222\n");
        let params = cfg.query("example.com");

        let url = SvnUrl::parse("svn+ssh://example.com:2200/repo").unwrap();
        let ssh = SshConfig::new(SshAuth::None);
        let settings =
            resolve_ssh_settings(&url, &ssh, Duration::from_secs(30), Some(&params)).unwrap();
        assert_eq!(settings.connect_port, 2200);
    }

    #[test]
    fn openssh_config_host_key_alias_is_used_for_known_hosts_lookup() {
        let cfg = OpenSshConfig::parse_str("Host example.com\n  HostKeyAlias alias\n");
        let params = cfg.query("example.com");

        let url = SvnUrl::parse("svn+ssh://example.com/repo").unwrap();
        let ssh = SshConfig::new(SshAuth::None);
        let settings =
            resolve_ssh_settings(&url, &ssh, Duration::from_secs(30), Some(&params)).unwrap();
        assert_eq!(settings.known_hosts_host, "alias");
    }

    #[test]
    fn openssh_config_strict_host_key_checking_no_disables_verification() {
        let cfg = OpenSshConfig::parse_str("Host example.com\n  StrictHostKeyChecking no\n");
        let params = cfg.query("example.com");

        let url = SvnUrl::parse("svn+ssh://example.com/repo").unwrap();
        let ssh = SshConfig::new(SshAuth::None);
        let settings =
            resolve_ssh_settings(&url, &ssh, Duration::from_secs(30), Some(&params)).unwrap();
        assert!(matches!(settings.host_key, SshHostKeyPolicy::AcceptAny));
        assert!(!settings.accept_new_host_keys);
    }

    #[test]
    fn openssh_config_strict_host_key_checking_accept_new_enables_learning() {
        let cfg =
            OpenSshConfig::parse_str("Host example.com\n  StrictHostKeyChecking accept-new\n");
        let params = cfg.query("example.com");

        let url = SvnUrl::parse("svn+ssh://example.com/repo").unwrap();
        let ssh = SshConfig::new(SshAuth::None);
        let settings =
            resolve_ssh_settings(&url, &ssh, Duration::from_secs(30), Some(&params)).unwrap();
        assert!(settings.accept_new_host_keys);
    }
}
