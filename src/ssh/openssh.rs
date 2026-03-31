use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::OnceCell;
use tracing::debug;

#[derive(Clone, Debug, Default)]
pub(super) struct HostParams {
    pub(super) host_name: Option<String>,
    pub(super) port: Option<u16>,
    pub(super) user: Option<String>,
    pub(super) identity_file: Option<Vec<PathBuf>>,
    pub(super) connect_timeout: Option<Duration>,
    pub(super) host_key_alias: Option<String>,
    pub(super) user_known_hosts_file: Option<String>,
    pub(super) strict_host_key_checking: Option<String>,
    pub(super) identity_agent: Option<String>,
    pub(super) identities_only: Option<bool>,
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
pub(super) struct OpenSshConfig {
    blocks: Vec<HostBlock>,
}

impl OpenSshConfig {
    fn parse_default_file() -> Result<Option<Self>, std::io::Error> {
        let Some(home) = home_dir() else {
            return Ok(None);
        };
        let path = home.join(".ssh").join("config");
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
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
    pub(super) fn parse_str(input: &str) -> Self {
        let mut parser = OpenSshParser::new();
        let mut include_stack = Vec::new();
        let _ = parser.parse_str(input, None, &mut include_stack, 0);
        Self {
            blocks: parser.blocks,
        }
    }

    pub(super) fn query(&self, host: &str) -> HostParams {
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

pub(super) async fn load_openssh_config() -> Option<&'static OpenSshConfig> {
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

    let mut pattern_idx = 0usize;
    let mut text_idx = 0usize;
    let mut star_idx = None::<usize>;
    let mut star_text_idx = 0usize;

    while text_idx < text.len() {
        if pattern_idx < pattern.len()
            && (pattern[pattern_idx] == b'?' || pattern[pattern_idx] == text[text_idx])
        {
            pattern_idx += 1;
            text_idx += 1;
            continue;
        }
        if pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
            star_idx = Some(pattern_idx);
            pattern_idx += 1;
            star_text_idx = text_idx;
            continue;
        }
        if let Some(saved_star_idx) = star_idx {
            pattern_idx = saved_star_idx + 1;
            star_text_idx += 1;
            text_idx = star_text_idx;
            continue;
        }
        return false;
    }

    while pattern_idx < pattern.len() && pattern[pattern_idx] == b'*' {
        pattern_idx += 1;
    }

    pattern_idx == pattern.len()
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
            Ok(paths) => paths,
            Err(_) => return Ok(()),
        };
        for include_path in include_paths {
            self.parse_file(&include_path, include_stack, include_depth + 1);
        }
        Ok(())
    }

    fn parse_file(&mut self, path: &Path, include_stack: &mut Vec<PathBuf>, depth: usize) {
        let path = match std::fs::canonicalize(path) {
            Ok(path) => path,
            Err(_) => path.to_path_buf(),
        };
        if include_stack.contains(&path) {
            return;
        }
        include_stack.push(path.clone());

        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
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
                    && let Some(value) = values.first()
                {
                    params.host_name = Some(value.to_string());
                }
            }
            "user" => {
                if params.user.is_none()
                    && let Some(value) = values.first()
                {
                    params.user = Some(value.to_string());
                }
            }
            "port" => {
                if params.port.is_none()
                    && let Some(value) = values.first()
                    && let Ok(port) = value.parse::<u16>()
                {
                    params.port = Some(port);
                }
            }
            "identityfile" => {
                if let Some(value) = values.first() {
                    params
                        .identity_file
                        .get_or_insert_with(Vec::new)
                        .push(PathBuf::from(value));
                }
            }
            "identityagent" => {
                if params.identity_agent.is_none()
                    && let Some(value) = values.first()
                {
                    params.identity_agent = Some(value.to_string());
                }
            }
            "identitiesonly" => {
                if params.identities_only.is_none()
                    && let Some(value) = values.first()
                    && let Some(enabled) = parse_bool(value)
                {
                    params.identities_only = Some(enabled);
                }
            }
            "hostkeyalias" => {
                if params.host_key_alias.is_none()
                    && let Some(value) = values.first()
                {
                    params.host_key_alias = Some(value.to_string());
                }
            }
            "userknownhostsfile" => {
                if params.user_known_hosts_file.is_none()
                    && let Some(value) = values.first()
                {
                    params.user_known_hosts_file = Some(value.to_string());
                }
            }
            "stricthostkeychecking" => {
                if params.strict_host_key_checking.is_none()
                    && let Some(value) = values.first()
                {
                    params.strict_host_key_checking = Some(value.to_string());
                }
            }
            "connecttimeout" => {
                if params.connect_timeout.is_none()
                    && let Some(value) = values.first()
                    && let Ok(secs) = value.parse::<u64>()
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

    if let Some((key, value)) = tokens[0].split_once('=')
        && !key.is_empty()
    {
        let key = key.to_string();
        let mut values = Vec::new();
        if !value.is_empty() {
            values.push(value.to_string());
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

    while let Some(ch) = chars.next() {
        if ch == '#' && !in_single && !in_double {
            break;
        }

        if ch == '\\' && !in_single {
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }

        if ch == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            continue;
        }

        if ch.is_whitespace() && !in_single && !in_double {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }

        current.push(ch);
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn parse_host_patterns(values: &[String]) -> Vec<HostPattern> {
    values
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let (negated, pattern) = value
                .strip_prefix('!')
                .map(|pattern| (true, pattern))
                .unwrap_or((false, value));
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
    let mut iter = values.iter().map(|value| value.as_str());
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

    if values
        .iter()
        .skip(1)
        .any(|value| is_match_criterion_keyword(value))
    {
        return None;
    }

    let patterns = parse_host_patterns(&values[1..]);
    (!patterns.is_empty()).then_some(patterns)
}

fn expand_include_paths(path: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let text = path.to_string_lossy();
    if !text.contains('*') && !text.contains('?') && !text.contains('[') {
        return Ok(vec![path.to_path_buf()]);
    }

    let Some(parent) = path.parent() else {
        return Ok(Vec::new());
    };
    let Some(file_pattern) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if wildcard_match(file_pattern, name) {
            out.push(entry.path());
        }
    }

    out.sort();
    Ok(out)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|path| !path.is_empty())
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

pub(super) fn expand_tilde_str(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\"))
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

pub(super) fn expand_tilde_path(path: &Path) -> PathBuf {
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

pub(super) fn normalize_identity_file_path(path: &Path) -> PathBuf {
    let path = expand_tilde_path(path);
    if path.is_relative()
        && let Some(home) = home_dir()
    {
        return home.join(path);
    }
    path
}

pub(super) fn default_identity_files() -> Vec<PathBuf> {
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
