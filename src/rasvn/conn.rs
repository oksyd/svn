use hmac::{Hmac, Mac};
use md5::Md5;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;
use tracing::debug;

use crate::{Capability, SvnError};

use super::SvnItem;
use super::encode_item;
use super::parse::{parse_repos_info, parse_server_error};
#[cfg(feature = "cyrus-sasl")]
use super::sasl::{CyrusSasl, SASL_CONTINUE, base64_decode, base64_encode};
use super::wire::encode_command_item;

type AuthMechanismChoice = (String, Option<Vec<u8>>);
type AuthMechanismChoices = Vec<AuthMechanismChoice>;

#[cfg(feature = "cyrus-sasl")]
trait SaslSecurityLayer: Send {
    fn max_outbuf(&self) -> u32;
    fn encode(&mut self, input: &[u8]) -> Result<Vec<u8>, SvnError>;
    fn decode(&mut self, input: &[u8]) -> Result<Vec<u8>, SvnError>;
}

#[cfg(feature = "cyrus-sasl")]
impl SaslSecurityLayer for CyrusSasl {
    fn max_outbuf(&self) -> u32 {
        CyrusSasl::max_outbuf(self)
    }

    fn encode(&mut self, input: &[u8]) -> Result<Vec<u8>, SvnError> {
        CyrusSasl::encode(self, input)
    }

    fn decode(&mut self, input: &[u8]) -> Result<Vec<u8>, SvnError> {
        CyrusSasl::decode(self, input)
    }
}

#[derive(Debug)]
pub(crate) struct CommandResponse {
    success: bool,
    params: Vec<SvnItem>,
    errors: Vec<SvnItem>,
}

impl CommandResponse {
    pub(crate) fn is_failure(&self) -> bool {
        !self.success
    }

    pub(crate) fn success_params(&self, ctx: &str) -> Result<&[SvnItem], SvnError> {
        if self.success {
            Ok(&self.params)
        } else {
            Err(self.failure(ctx))
        }
    }

    pub(crate) fn ensure_success(&self, ctx: &str) -> Result<(), SvnError> {
        let _ = self.success_params(ctx)?;
        Ok(())
    }

    pub(crate) fn failure(&self, ctx: &str) -> SvnError {
        SvnError::Server(self.failure_server_error().with_context(ctx.to_string()))
    }

    pub(crate) fn failure_server_error(&self) -> crate::ServerError {
        parse_server_error(&self.errors)
    }

    pub(crate) fn failure_message(&self) -> String {
        self.failure_server_error().message_summary()
    }
}

pub(crate) struct RaSvnConnectionConfig {
    pub(crate) username: Option<String>,
    pub(crate) password: Option<String>,
    #[cfg(feature = "cyrus-sasl")]
    pub(crate) host: String,
    #[cfg(feature = "cyrus-sasl")]
    pub(crate) local_addrport: Option<String>,
    #[cfg(feature = "cyrus-sasl")]
    pub(crate) remote_addrport: Option<String>,
    pub(crate) is_tunneled: bool,
    pub(crate) url: String,
    pub(crate) ra_client: String,
    pub(crate) read_timeout: Duration,
    pub(crate) write_timeout: Duration,
}

type DynRead = Box<dyn AsyncRead + Unpin + Send>;
type DynWrite = Box<dyn AsyncWrite + Unpin + Send>;

pub(crate) struct RaSvnConnection {
    read: DynRead,
    write: DynWrite,
    buf: Vec<u8>,
    pos: usize,
    write_buf: Vec<u8>,
    username: Option<String>,
    password: Option<String>,
    #[cfg(feature = "cyrus-sasl")]
    host: String,
    #[cfg(feature = "cyrus-sasl")]
    local_addrport: Option<String>,
    #[cfg(feature = "cyrus-sasl")]
    remote_addrport: Option<String>,
    is_tunneled: bool,
    url: String,
    ra_client: String,
    read_timeout: Duration,
    write_timeout: Duration,
    server_caps: Vec<String>,
    #[cfg(feature = "cyrus-sasl")]
    sasl: Option<Box<dyn SaslSecurityLayer>>,
}

impl RaSvnConnection {
    pub(crate) fn new(read: DynRead, write: DynWrite, config: RaSvnConnectionConfig) -> Self {
        Self {
            read,
            write,
            buf: Vec::new(),
            pos: 0,
            write_buf: Vec::new(),
            username: config.username,
            password: config.password,
            #[cfg(feature = "cyrus-sasl")]
            host: config.host,
            #[cfg(feature = "cyrus-sasl")]
            local_addrport: config.local_addrport,
            #[cfg(feature = "cyrus-sasl")]
            remote_addrport: config.remote_addrport,
            is_tunneled: config.is_tunneled,
            url: config.url,
            ra_client: config.ra_client,
            read_timeout: config.read_timeout,
            write_timeout: config.write_timeout,
            server_caps: Vec::new(),
            #[cfg(feature = "cyrus-sasl")]
            sasl: None,
        }
    }

    pub(crate) fn server_has_cap(&self, cap: &str) -> bool {
        self.server_caps.iter().any(|c| c == cap)
    }

    #[cfg(test)]
    pub(crate) fn set_server_caps_for_test(&mut self, caps: &[&str]) {
        self.server_caps = caps.iter().map(|cap| (*cap).to_string()).collect();
    }

    pub(crate) fn set_session_url(&mut self, url: String) {
        self.url = url;
    }

    pub(crate) async fn handshake(&mut self) -> Result<crate::ServerInfo, SvnError> {
        if self.is_tunneled {
            self.skip_leading_garbage().await?;
        }
        let greeting = self.read_command_response().await?;
        let params = greeting.success_params("greeting")?;
        if params.len() < 4 {
            return Err(SvnError::Protocol("greeting params too short".into()));
        }
        let minver = params[0]
            .as_u64()
            .ok_or_else(|| SvnError::Protocol("invalid greeting minver".into()))?;
        let maxver = params[1]
            .as_u64()
            .ok_or_else(|| SvnError::Protocol("invalid greeting maxver".into()))?;
        let caps: Vec<String> = params
            .get(3)
            .and_then(|i| i.as_list())
            .map(|server_caps| {
                server_caps
                    .into_iter()
                    .filter_map(|c| c.as_word())
                    .collect()
            })
            .unwrap_or_default();
        self.server_caps = caps.clone();
        debug!(minver, maxver, caps = ?caps, "received server greeting");
        if !(minver <= 2 && 2 <= maxver) {
            return Err(SvnError::Protocol(format!(
                "server does not support protocol v2 (min={minver}, max={maxver})"
            )));
        }
        if !self.server_has_cap(Capability::EditPipeline.as_wire_word()) {
            return Err(SvnError::Protocol(
                "server does not support edit pipelining".into(),
            ));
        }

        debug!(url = %self.url, ra_client = %self.ra_client, "sending client greeting response");
        let client_caps = [
            Capability::EditPipeline,
            Capability::Svndiff1,
            Capability::AcceptsSvndiff2,
            Capability::AbsentEntries,
            Capability::Depth,
            Capability::MergeInfo,
            Capability::LogRevProps,
        ];
        let client_cap_items = client_caps
            .into_iter()
            .map(|cap| SvnItem::Word(cap.as_wire_word().to_string()))
            .collect();
        let response = SvnItem::List(vec![
            SvnItem::Number(2),
            SvnItem::List(client_cap_items),
            SvnItem::String(self.url.as_bytes().to_vec()),
            SvnItem::String(self.ra_client.as_bytes().to_vec()),
            SvnItem::List(Vec::new()),
        ]);
        self.write_item(&response).await?;

        self.handle_auth_request_initial().await?;

        let repos_info = self.read_command_response().await?;
        let params = repos_info.success_params("repos-info")?;
        let repository = parse_repos_info(params)?;
        for cap in &repository.capabilities {
            if !self.server_caps.iter().any(|c| c == cap) {
                self.server_caps.push(cap.clone());
            }
        }
        debug!("handshake complete");
        Ok(crate::ServerInfo {
            server_caps: self.server_caps.clone(),
            repository,
        })
    }

    async fn skip_leading_garbage(&mut self) -> Result<(), SvnError> {
        if self.pos < self.buf.len() {
            let mut saw_lparen = false;
            for i in self.pos..self.buf.len() {
                let b = self.buf[i];
                if saw_lparen && b.is_ascii_whitespace() {
                    let rest = self.buf[i..].to_vec();
                    self.buf.clear();
                    self.buf.push(b'(');
                    self.buf.extend_from_slice(&rest);
                    self.pos = 0;
                    return Ok(());
                }
                saw_lparen = b == b'(';
            }
        }

        self.buf.clear();
        self.pos = 0;

        const MAX_GARBAGE: usize = 64 * 1024;
        const PREVIEW_MAX: usize = 1024;
        let mut preview = Vec::<u8>::new();

        let mut temp = [0u8; 256];
        let mut total_discarded = 0usize;
        let mut saw_lparen = false;
        loop {
            let n = tokio::time::timeout(self.read_timeout, self.read.read(&mut temp))
                .await
                .map_err(|_| {
                    SvnError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "read timed out",
                    ))
                })??;
            if n == 0 {
                return Err(SvnError::Protocol("unexpected EOF".into()));
            }

            if preview.len() < PREVIEW_MAX {
                let take = (PREVIEW_MAX - preview.len()).min(n);
                preview.extend_from_slice(&temp[..take]);
            }

            total_discarded = total_discarded.saturating_add(n);
            if total_discarded > MAX_GARBAGE {
                let preview = String::from_utf8_lossy(&preview).to_string();
                return Err(SvnError::Protocol(format!(
                    "tunnel produced non-svn output before greeting; discarded >{MAX_GARBAGE} bytes; start of output: {preview:?}"
                )));
            }

            for (idx, b) in temp[..n].iter().copied().enumerate() {
                if saw_lparen && b.is_ascii_whitespace() {
                    self.buf.push(b'(');
                    self.buf.extend_from_slice(&temp[idx..n]);
                    self.pos = 0;
                    return Ok(());
                }
                saw_lparen = b == b'(';
            }
        }
    }

    pub(crate) async fn call(
        &mut self,
        command: &str,
        params: SvnItem,
    ) -> Result<CommandResponse, SvnError> {
        self.send_command(command, params).await?;
        self.handle_auth_request().await?;
        self.read_command_response().await
    }

    pub(crate) async fn send_command(
        &mut self,
        command: &str,
        params: SvnItem,
    ) -> Result<(), SvnError> {
        self.write_buf.clear();
        encode_command_item(command, &params, &mut self.write_buf);
        self.write_buf.push(b'\n');

        let buf = std::mem::take(&mut self.write_buf);
        let result = self.write_wire_bytes(&buf).await;
        self.write_buf = buf;
        result
    }

    pub(crate) async fn handle_auth_request_initial(&mut self) -> Result<(), SvnError> {
        let auth_req = self.read_command_response().await?;
        self.handle_auth_request_response(&auth_req).await
    }

    pub(crate) async fn handle_auth_request(&mut self) -> Result<(), SvnError> {
        let auth_req = self.read_command_response().await?;
        self.handle_auth_request_response(&auth_req).await
    }

    async fn handle_auth_request_response(
        &mut self,
        auth_req: &CommandResponse,
    ) -> Result<(), SvnError> {
        if auth_req.is_failure() {
            if let Some(first) = auth_req.errors.first().and_then(|e| e.as_list())
                && first.len() >= 4
            {
                let code = first[0].as_u64().unwrap_or_default();
                let message = first[1]
                    .as_string()
                    .unwrap_or_else(|| "<non-utf8>".to_string());
                let file = first[2]
                    .as_string()
                    .unwrap_or_else(|| "<non-utf8>".to_string());
                let line = first[3].as_u64().unwrap_or_default();
                debug!(code, message = %message, file = %file, line, "auth-request failed");
            }
            debug!(
                message = %auth_req.failure_message(),
                "auth-request command response is failure"
            );
        }
        let params = auth_req.success_params("auth-request")?;
        if params.len() < 2 {
            return Err(SvnError::Protocol("auth-request params too short".into()));
        }
        let mechs = params[0]
            .as_list()
            .ok_or_else(|| SvnError::Protocol("auth mechs not a list".into()))?;
        if mechs.is_empty() {
            debug!("auth-request has empty mechanism list (no auth required)");
            return Ok(());
        }
        let realm = params[1]
            .as_string()
            .unwrap_or_else(|| "<unknown>".to_string());
        debug!(realm = %realm, "server requires authentication");

        let mech_words: Vec<String> = mechs.into_iter().filter_map(|m| m.as_word()).collect();
        debug!(mechs = ?mech_words, "auth mechanisms offered");

        match self.handle_auth_request_builtin(&mech_words).await {
            Ok(()) => Ok(()),
            Err(builtin_err) => {
                #[cfg(feature = "cyrus-sasl")]
                {
                    if matches!(
                        &builtin_err,
                        SvnError::AuthUnavailable | SvnError::AuthFailed(_)
                    ) {
                        match self.handle_auth_request_cyrus_sasl(&mech_words).await {
                            Ok(()) => Ok(()),
                            Err(SvnError::AuthUnavailable) => Err(builtin_err),
                            Err(err) => Err(err),
                        }
                    } else {
                        Err(builtin_err)
                    }
                }

                #[cfg(not(feature = "cyrus-sasl"))]
                {
                    Err(builtin_err)
                }
            }
        }
    }

    async fn handle_auth_request_builtin(&mut self, mechs: &[String]) -> Result<(), SvnError> {
        let mechs_to_try = self.select_mechs(mechs)?;
        let mut last_failure = None::<String>;

        for (mech, initial) in mechs_to_try {
            debug!(mech = %mech, "trying auth mechanism");

            let token_tuple = match initial {
                Some(token) => SvnItem::List(vec![SvnItem::String(token)]),
                None => SvnItem::List(Vec::new()),
            };
            self.write_item(&SvnItem::List(vec![
                SvnItem::Word(mech.clone()),
                token_tuple,
            ]))
            .await?;

            loop {
                let challenge = self.read_item().await?;
                let SvnItem::List(parts) = challenge else {
                    return Err(SvnError::Protocol("invalid auth challenge".into()));
                };
                let Some(kind) = parts.first().and_then(|i| i.as_word()) else {
                    return Err(SvnError::Protocol("invalid auth challenge kind".into()));
                };
                match kind.as_str() {
                    "step" => {
                        debug!(mech = %mech, "auth challenge step");
                        let token = parts
                            .get(1)
                            .and_then(|i| i.as_list())
                            .and_then(|list| list.first().and_then(|i| i.as_bytes_string()))
                            .ok_or_else(|| SvnError::Protocol("auth step missing token".into()))?;
                        let reply = self.auth_step_reply(&mech, token)?;
                        self.write_item(&SvnItem::String(reply)).await?;
                    }
                    "success" => return Ok(()),
                    "failure" => {
                        let message = parts
                            .get(1)
                            .and_then(|i| i.as_list())
                            .and_then(|list| list.first().and_then(|i| i.as_string()))
                            .unwrap_or_else(|| "auth failed".to_string());
                        debug!(mech = %mech, message = %message, "auth mechanism failed");
                        last_failure = Some(message);
                        break;
                    }
                    other => {
                        return Err(SvnError::Protocol(format!(
                            "unexpected auth challenge: {other}"
                        )));
                    }
                }
            }
        }

        Err(SvnError::AuthFailed(
            last_failure.unwrap_or_else(|| "auth failed".to_string()),
        ))
    }

    #[cfg(feature = "cyrus-sasl")]
    async fn handle_auth_request_cyrus_sasl(&mut self, mechs: &[String]) -> Result<(), SvnError> {
        let mechstring = if mechs.iter().any(|m| m == "EXTERNAL") {
            "EXTERNAL".to_string()
        } else if mechs.iter().any(|m| m == "ANONYMOUS") {
            "ANONYMOUS".to_string()
        } else {
            mechs.join(" ")
        };

        let mechlist = std::ffi::CString::new(mechstring)
            .map_err(|_| SvnError::Protocol("SASL mech list contains NUL byte".into()))?;
        let mechlist = mechlist.as_c_str();

        let mut sasl = CyrusSasl::new(
            &self.host,
            self.username.as_deref(),
            self.password.as_deref(),
            false,
            self.local_addrport.as_deref(),
            self.remote_addrport.as_deref(),
        )?;

        let (mech, initial, mut rc) = sasl.client_start(mechlist)?;

        let mut initial_token = None;
        if initial.is_some() || mech == "EXTERNAL" {
            let raw = initial.unwrap_or_default();
            initial_token = Some(base64_encode(&raw));
        }

        let token_tuple = match initial_token {
            Some(token) => SvnItem::List(vec![SvnItem::String(token)]),
            None => SvnItem::List(Vec::new()),
        };
        self.write_item(&SvnItem::List(vec![
            SvnItem::Word(mech.clone()),
            token_tuple,
        ]))
        .await?;

        let mut last_status = None::<String>;
        while rc == SASL_CONTINUE {
            let challenge = self.read_item().await?;
            let SvnItem::List(parts) = challenge else {
                return Err(SvnError::Protocol("invalid SASL challenge".into()));
            };
            let Some(kind) = parts.first().and_then(|i| i.as_word()) else {
                return Err(SvnError::Protocol("invalid SASL challenge kind".into()));
            };
            last_status = Some(kind.clone());

            match kind.as_str() {
                "failure" => {
                    let message = parts
                        .get(1)
                        .and_then(|i| i.as_list())
                        .and_then(|list| list.first().and_then(|i| i.as_string()))
                        .unwrap_or_else(|| "auth failed".to_string());
                    return Err(SvnError::AuthFailed(message));
                }
                "success" | "step" => {}
                other => {
                    return Err(SvnError::Protocol(format!(
                        "unexpected SASL challenge: {other}"
                    )));
                }
            }

            let token = parts
                .get(1)
                .and_then(|i| i.as_list())
                .and_then(|list| list.first().and_then(|i| i.as_bytes_string()))
                .ok_or_else(|| SvnError::Protocol("SASL step missing token".into()))?;
            let token = if mech == "CRAM-MD5" {
                token
            } else {
                base64_decode(&token)?
            };

            let (out, next_rc) = sasl.client_step(&token)?;
            rc = next_rc;

            if kind == "success" {
                break;
            }

            let out = out.unwrap_or_default();
            let out = if mech == "CRAM-MD5" {
                out
            } else {
                base64_encode(&out)
            };
            self.write_item(&SvnItem::String(out)).await?;
        }

        if !matches!(last_status.as_deref(), Some("success")) {
            let item = self.read_item().await?;
            let SvnItem::List(parts) = item else {
                return Err(SvnError::Protocol("invalid SASL final response".into()));
            };
            let Some(kind) = parts.first().and_then(|i| i.as_word()) else {
                return Err(SvnError::Protocol("invalid SASL final kind".into()));
            };
            match kind.as_str() {
                "success" => {}
                "failure" => {
                    let message = parts
                        .get(1)
                        .and_then(|i| i.as_list())
                        .and_then(|list| list.first().and_then(|i| i.as_string()))
                        .unwrap_or_else(|| "auth failed".to_string());
                    return Err(SvnError::AuthFailed(message));
                }
                other => {
                    return Err(SvnError::Protocol(format!(
                        "unexpected SASL final response: {other}"
                    )));
                }
            }
        }

        let ssf = sasl.ssf()?;
        if ssf > 0 {
            if self.pos < self.buf.len() {
                let encrypted = self.buf[self.pos..].to_vec();
                self.buf.clear();
                self.pos = 0;

                let decoded = sasl.decode(&encrypted)?;
                self.buf.extend_from_slice(&decoded);
            } else if self.pos > 0 {
                self.buf.clear();
                self.pos = 0;
            }

            self.sasl = Some(Box::new(sasl));
        }
        Ok(())
    }

    fn select_mechs(&self, mechs: &[String]) -> Result<AuthMechanismChoices, SvnError> {
        let has_user = self.username.as_ref().is_some_and(|u| !u.trim().is_empty());
        let has_pass = self.password.as_ref().is_some();

        let mut out = Vec::new();

        if self.is_tunneled && mechs.iter().any(|m| m == "EXTERNAL") {
            out.push(("EXTERNAL".to_string(), Some(Vec::new())));
        }

        if has_user && has_pass && mechs.iter().any(|m| m == "CRAM-MD5") {
            out.push(("CRAM-MD5".to_string(), None));
        }

        if has_user && has_pass && mechs.iter().any(|m| m == "PLAIN") {
            let user = self.username.clone().unwrap_or_default();
            let pass = self.password.clone().unwrap_or_default();
            let mut token = Vec::with_capacity(user.len() + pass.len() + 2);
            token.push(0);
            token.extend_from_slice(user.as_bytes());
            token.push(0);
            token.extend_from_slice(pass.as_bytes());
            out.push(("PLAIN".to_string(), Some(token)));
        }

        if mechs.iter().any(|m| m == "ANONYMOUS") {
            out.push(("ANONYMOUS".to_string(), Some(Vec::new())));
        }

        if out.is_empty() && mechs.iter().any(|m| m == "EXTERNAL") {
            out.push(("EXTERNAL".to_string(), Some(Vec::new())));
        }

        if out.is_empty() {
            Err(SvnError::AuthUnavailable)
        } else {
            Ok(out)
        }
    }

    #[cfg(test)]
    fn select_mech(&self, mechs: &[String]) -> Result<AuthMechanismChoice, SvnError> {
        let Some(choice) = self.select_mechs(mechs)?.into_iter().next() else {
            return Err(SvnError::AuthUnavailable);
        };
        Ok(choice)
    }

    fn auth_step_reply(&self, mech: &str, challenge: Vec<u8>) -> Result<Vec<u8>, SvnError> {
        match mech {
            "CRAM-MD5" => {
                let user = self
                    .username
                    .clone()
                    .ok_or_else(|| SvnError::AuthFailed("missing username".into()))?;
                let pass = self
                    .password
                    .clone()
                    .ok_or_else(|| SvnError::AuthFailed("missing password".into()))?;
                let mut mac = Hmac::<Md5>::new_from_slice(pass.as_bytes())
                    .map_err(|_| SvnError::Protocol("failed to create HMAC-MD5".into()))?;
                mac.update(&challenge);
                let digest = mac.finalize().into_bytes();
                let hex = hex::encode(digest);
                Ok(format!("{user} {hex}").into_bytes())
            }
            "PLAIN" | "ANONYMOUS" | "EXTERNAL" => Err(SvnError::Protocol(format!(
                "unexpected auth step for {mech}"
            ))),
            other => Err(SvnError::Protocol(format!(
                "auth step not implemented for {other}"
            ))),
        }
    }

    pub(crate) async fn write_cmd_success(&mut self) -> Result<(), SvnError> {
        self.write_item(&SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]))
        .await
    }

    pub(crate) async fn write_cmd_failure_early(
        &mut self,
        err: &SvnError,
    ) -> Result<bool, SvnError> {
        let message = err.to_string();
        let item = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(1),
                SvnItem::String(message.into_bytes()),
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
            ])]),
        ]);

        let mut buf = Vec::new();
        encode_item(&item, &mut buf);
        buf.push(b'\n');

        #[cfg(feature = "cyrus-sasl")]
        let wire = if let Some(sasl) = self.sasl.as_mut() {
            let max = sasl.max_outbuf() as usize;
            let mut out = Vec::new();
            let mut offset = 0usize;
            while offset < buf.len() {
                let take = if max == 0 {
                    buf.len() - offset
                } else {
                    (buf.len() - offset).min(max)
                };
                let chunk = &buf[offset..offset + take];
                out.extend_from_slice(&sasl.encode(chunk)?);
                offset += take;
            }
            out
        } else {
            buf
        };

        #[cfg(not(feature = "cyrus-sasl"))]
        let wire = buf;

        let deadline = Instant::now() + self.write_timeout;
        let mut offset = 0usize;
        let mut done = false;
        while offset < wire.len() {
            if Instant::now() >= deadline {
                return Err(SvnError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "write timed out",
                )));
            }

            match tokio::time::timeout(Duration::from_millis(0), self.write.write(&wire[offset..]))
                .await
            {
                Ok(Ok(0)) => {
                    return Err(SvnError::Io(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "write returned 0 bytes",
                    )));
                }
                Ok(Ok(n)) => offset += n,
                Ok(Err(err)) => return Err(SvnError::Io(err)),
                Err(_) => {
                    if !self.data_available().await? {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        continue;
                    }

                    let item = self.read_item().await?;
                    if let SvnItem::List(parts) = item
                        && let Some(cmd) = parts.first().and_then(|i| i.as_word())
                        && cmd == "abort-edit"
                    {
                        done = true;
                    }
                }
            }
        }

        self.write.flush().await?;
        Ok(done)
    }

    pub(crate) async fn write_wire_bytes(&mut self, cleartext: &[u8]) -> Result<(), SvnError> {
        #[cfg(feature = "cyrus-sasl")]
        if let Some(sasl) = self.sasl.as_mut() {
            let max = sasl.max_outbuf() as usize;
            let mut offset = 0usize;
            while offset < cleartext.len() {
                let take = if max == 0 {
                    cleartext.len() - offset
                } else {
                    (cleartext.len() - offset).min(max)
                };
                let chunk = &cleartext[offset..offset + take];
                let encoded = sasl.encode(chunk)?;
                tokio::time::timeout(self.write_timeout, self.write.write_all(&encoded))
                    .await
                    .map_err(|_| {
                        SvnError::Io(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "write timed out",
                        ))
                    })??;
                offset += take;
            }
        } else {
            tokio::time::timeout(self.write_timeout, self.write.write_all(cleartext))
                .await
                .map_err(|_| {
                    SvnError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "write timed out",
                    ))
                })??;
        }

        #[cfg(not(feature = "cyrus-sasl"))]
        {
            tokio::time::timeout(self.write_timeout, self.write.write_all(cleartext))
                .await
                .map_err(|_| {
                    SvnError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "write timed out",
                    ))
                })??;
        }

        self.write.flush().await?;
        Ok(())
    }

    async fn write_item(&mut self, item: &SvnItem) -> Result<(), SvnError> {
        self.write_buf.clear();
        encode_item(item, &mut self.write_buf);
        self.write_buf.push(b'\n');

        let buf = std::mem::take(&mut self.write_buf);
        let result = self.write_wire_bytes(&buf).await;
        self.write_buf = buf;
        result
    }

    pub(crate) async fn read_command_response(&mut self) -> Result<CommandResponse, SvnError> {
        let item = self.read_item().await?;
        let SvnItem::List(parts) = item else {
            return Err(SvnError::Protocol("command response not a list".into()));
        };
        if parts.is_empty() {
            return Err(SvnError::Protocol("empty command response".into()));
        }
        let kind = parts[0]
            .as_word()
            .ok_or_else(|| SvnError::Protocol("command response kind not a word".into()))?;
        match kind.as_str() {
            "success" => {
                let params = parts.get(1).and_then(|i| i.as_list()).unwrap_or_default();
                Ok(CommandResponse {
                    success: true,
                    params,
                    errors: Vec::new(),
                })
            }
            "failure" => {
                let errs = parts.get(1).and_then(|i| i.as_list()).unwrap_or_default();
                Ok(CommandResponse {
                    success: false,
                    params: Vec::new(),
                    errors: errs,
                })
            }
            other => Err(SvnError::Protocol(format!(
                "unexpected command response kind: {other}"
            ))),
        }
    }

    pub(crate) async fn read_item(&mut self) -> Result<SvnItem, SvnError> {
        tokio::time::timeout(self.read_timeout, self.read_item_inner())
            .await
            .map_err(|_| {
                SvnError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "read timed out",
                ))
            })?
    }

    pub(crate) async fn data_available(&mut self) -> Result<bool, SvnError> {
        while self.pos < self.buf.len() && self.buf[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        if self.pos < self.buf.len() {
            return Ok(true);
        }
        if self.pos > 0 {
            let len = self.buf.len();
            self.buf.copy_within(self.pos..len, 0);
            self.buf.truncate(len - self.pos);
            self.pos = 0;
        }

        let mut temp = [0u8; 16384];
        match tokio::time::timeout(Duration::from_millis(0), self.read.read(&mut temp)).await {
            Ok(Ok(n)) => {
                if n == 0 {
                    return Err(SvnError::Protocol("unexpected EOF".into()));
                }

                #[cfg(feature = "cyrus-sasl")]
                if let Some(sasl) = self.sasl.as_mut() {
                    let decoded = sasl.decode(&temp[..n])?;
                    self.buf.extend_from_slice(&decoded);
                } else {
                    self.buf.extend_from_slice(&temp[..n]);
                }

                #[cfg(not(feature = "cyrus-sasl"))]
                {
                    self.buf.extend_from_slice(&temp[..n]);
                }

                while self.pos < self.buf.len() && self.buf[self.pos].is_ascii_whitespace() {
                    self.pos += 1;
                }
                if self.pos < self.buf.len() {
                    Ok(true)
                } else {
                    if self.pos > 0 {
                        let len = self.buf.len();
                        self.buf.copy_within(self.pos..len, 0);
                        self.buf.truncate(len - self.pos);
                        self.pos = 0;
                    }
                    Ok(false)
                }
            }
            Ok(Err(err)) => Err(SvnError::Io(err)),
            Err(_) => Ok(false),
        }
    }

    async fn read_item_inner(&mut self) -> Result<SvnItem, SvnError> {
        skip_ws(self).await?;
        let ch = self.peek_byte().await?;
        if ch == b'(' {
            return self.read_list().await;
        }
        self.read_atom().await
    }

    async fn read_list(&mut self) -> Result<SvnItem, SvnError> {
        self.consume_byte().await?;
        require_ws(self).await?;

        let mut stack: Vec<Vec<SvnItem>> = vec![Vec::new()];
        loop {
            skip_ws(self).await?;
            let next = self.peek_byte().await?;
            match next {
                b')' => {
                    self.consume_byte().await?;
                    require_ws(self).await?;

                    let completed = stack
                        .pop()
                        .ok_or_else(|| SvnError::Protocol("list stack underflow".into()))?;
                    let item = SvnItem::List(completed);
                    if let Some(parent) = stack.last_mut() {
                        parent.push(item);
                    } else {
                        return Ok(item);
                    }
                }
                b'(' => {
                    self.consume_byte().await?;
                    require_ws(self).await?;
                    stack.push(Vec::new());
                }
                _ => {
                    let atom = self.read_atom().await?;
                    stack
                        .last_mut()
                        .ok_or_else(|| SvnError::Protocol("list stack underflow".into()))?
                        .push(atom);
                }
            }
        }
    }

    async fn read_atom(&mut self) -> Result<SvnItem, SvnError> {
        skip_ws(self).await?;
        let ch = self.peek_byte().await?;
        match ch {
            b'0'..=b'9' => {
                let n = parse_digits(self).await?;
                let next = self.peek_byte().await?;
                if next == b':' {
                    self.consume_byte().await?;
                    let bytes = self.read_exact_vec(n as usize).await?;
                    require_ws(self).await?;
                    Ok(SvnItem::String(bytes))
                } else {
                    require_ws(self).await?;
                    Ok(SvnItem::Number(n))
                }
            }
            _ => {
                let word = parse_word(self).await?;
                let item = match word.as_str() {
                    "true" => SvnItem::Bool(true),
                    "false" => SvnItem::Bool(false),
                    _ => SvnItem::Word(word),
                };
                require_ws(self).await?;
                Ok(item)
            }
        }
    }

    async fn read_exact_vec(&mut self, n: usize) -> Result<Vec<u8>, SvnError> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            if self.pos < self.buf.len() {
                let take = (n - out.len()).min(self.buf.len() - self.pos);
                out.extend_from_slice(&self.buf[self.pos..self.pos + take]);
                self.pos += take;
            } else {
                self.fill().await?;
            }
        }
        Ok(out)
    }

    async fn fill(&mut self) -> Result<(), SvnError> {
        if self.pos > 0 {
            let len = self.buf.len();
            self.buf.copy_within(self.pos..len, 0);
            self.buf.truncate(len - self.pos);
            self.pos = 0;
        }
        let mut temp = [0u8; 16384];

        #[cfg(feature = "cyrus-sasl")]
        {
            loop {
                let n = self.read.read(&mut temp).await?;
                if n == 0 {
                    return Err(SvnError::Protocol("unexpected EOF".into()));
                }

                if let Some(sasl) = self.sasl.as_mut() {
                    let decoded = sasl.decode(&temp[..n])?;
                    if decoded.is_empty() {
                        continue;
                    }
                    self.buf.extend_from_slice(&decoded);
                    break;
                }

                self.buf.extend_from_slice(&temp[..n]);
                break;
            }
            Ok(())
        }

        #[cfg(not(feature = "cyrus-sasl"))]
        {
            let n = self.read.read(&mut temp).await?;
            if n == 0 {
                return Err(SvnError::Protocol("unexpected EOF".into()));
            }
            self.buf.extend_from_slice(&temp[..n]);
            Ok(())
        }
    }

    async fn peek_byte(&mut self) -> Result<u8, SvnError> {
        loop {
            if self.pos < self.buf.len() {
                return Ok(self.buf[self.pos]);
            }
            self.fill().await?;
        }
    }

    async fn consume_byte(&mut self) -> Result<u8, SvnError> {
        let b = self.peek_byte().await?;
        self.pos += 1;
        Ok(b)
    }
}

async fn skip_ws(conn: &mut RaSvnConnection) -> Result<(), SvnError> {
    loop {
        let b = conn.peek_byte().await?;
        if b.is_ascii_whitespace() {
            let _ = conn.consume_byte().await?;
            continue;
        }
        break;
    }
    Ok(())
}

async fn require_ws(conn: &mut RaSvnConnection) -> Result<(), SvnError> {
    let b = conn.consume_byte().await?;
    if b.is_ascii_whitespace() {
        Ok(())
    } else {
        Err(SvnError::Protocol("expected whitespace".into()))
    }
}

async fn parse_digits(conn: &mut RaSvnConnection) -> Result<u64, SvnError> {
    let mut n = 0u64;
    loop {
        let b = conn.peek_byte().await?;
        if !b.is_ascii_digit() {
            break;
        }
        let _ = conn.consume_byte().await?;
        n = n
            .checked_mul(10)
            .and_then(|v| v.checked_add((b - b'0') as u64))
            .ok_or_else(|| SvnError::Protocol("number overflow".into()))?;
    }
    Ok(n)
}

async fn parse_word(conn: &mut RaSvnConnection) -> Result<String, SvnError> {
    let mut bytes = Vec::new();
    loop {
        let b = conn.peek_byte().await?;
        if b.is_ascii_whitespace() {
            break;
        }
        if b == b'(' || b == b')' || b == b':' {
            return Err(SvnError::Protocol("invalid word token".into()));
        }
        bytes.push(conn.consume_byte().await?);
    }
    String::from_utf8(bytes).map_err(|_| SvnError::Protocol("non-utf8 word".into()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use proptest::prelude::*;
    use std::future::Future;
    #[cfg(feature = "cyrus-sasl")]
    use std::sync::{Arc, Mutex};

    fn run_async<T>(f: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    async fn connected_conn_inner(
        username: Option<String>,
        password: Option<String>,
        is_tunneled: bool,
    ) -> (RaSvnConnection, tokio::net::TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept_task = tokio::spawn(async move { listener.accept().await });
        let client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (server, _) = accept_task.await.unwrap().unwrap();

        let (read, write) = client.into_split();
        let conn = RaSvnConnection::new(
            Box::new(read),
            Box::new(write),
            RaSvnConnectionConfig {
                username,
                password,
                #[cfg(feature = "cyrus-sasl")]
                host: "example.com".to_string(),
                #[cfg(feature = "cyrus-sasl")]
                local_addrport: None,
                #[cfg(feature = "cyrus-sasl")]
                remote_addrport: None,
                is_tunneled,
                url: if is_tunneled {
                    "svn+ssh://example.com:22/repo".to_string()
                } else {
                    "svn://example.com:3690/repo".to_string()
                },
                ra_client: "test-ra_svn".to_string(),
                read_timeout: Duration::from_secs(1),
                write_timeout: Duration::from_secs(1),
            },
        );

        (conn, server)
    }

    async fn connected_conn(
        username: Option<String>,
        password: Option<String>,
    ) -> (RaSvnConnection, tokio::net::TcpStream) {
        connected_conn_inner(username, password, false).await
    }

    async fn connected_conn_tunneled(
        username: Option<String>,
        password: Option<String>,
    ) -> (RaSvnConnection, tokio::net::TcpStream) {
        connected_conn_inner(username, password, true).await
    }

    async fn write_item_line(stream: &mut tokio::net::TcpStream, item: &SvnItem) {
        let mut buf = Vec::new();
        encode_item(item, &mut buf);
        buf.push(b'\n');
        stream.write_all(&buf).await.unwrap();
        stream.flush().await.unwrap();
    }

    async fn read_until_newline(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut temp = [0u8; 1024];
        loop {
            let n = stream.read(&mut temp).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&temp[..n]);
            if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                buf.truncate(pos + 1);
                break;
            }
        }
        buf
    }

    fn arb_word() -> impl Strategy<Value = String> {
        "[A-Za-z_][A-Za-z0-9_\\-]{0,31}"
            .prop_filter("avoid bool words", |w| w != "true" && w != "false")
    }

    fn arb_item() -> impl Strategy<Value = SvnItem> {
        let leaf = prop_oneof![
            arb_word().prop_map(SvnItem::Word),
            any::<u64>().prop_map(SvnItem::Number),
            any::<bool>().prop_map(SvnItem::Bool),
            prop::collection::vec(any::<u8>(), 0..64).prop_map(SvnItem::String),
        ];
        leaf.prop_recursive(6, 256, 12, |inner| {
            prop::collection::vec(inner, 0..16).prop_map(SvnItem::List)
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 64,
            .. ProptestConfig::default()
        })]

        #[test]
        fn encode_then_read_roundtrips(item in arb_item()) {
            run_async(async {
                let (mut conn, mut server) = connected_conn(None, None).await;

                let mut encoded = Vec::new();
                encode_item(&item, &mut encoded);
                encoded.push(b'\n');

                server.write_all(&encoded).await.unwrap();
                server.flush().await.unwrap();

                let parsed = conn.read_item().await.unwrap();
                assert_eq!(parsed, item);
            });
        }
    }

    #[cfg(feature = "cyrus-sasl")]
    fn invert_bytes(bytes: &[u8]) -> Vec<u8> {
        bytes.iter().map(|b| b ^ 0xFF).collect()
    }

    #[cfg(feature = "cyrus-sasl")]
    fn chunk_sizes(total: usize, max: usize) -> Vec<usize> {
        if total == 0 {
            return Vec::new();
        }
        if max == 0 {
            return vec![total];
        }
        let mut out = Vec::new();
        let mut remaining = total;
        while remaining > 0 {
            let take = remaining.min(max);
            out.push(take);
            remaining -= take;
        }
        out
    }

    #[cfg(feature = "cyrus-sasl")]
    struct DummySecurityLayer {
        max: u32,
        encode_calls: Arc<Mutex<Vec<usize>>>,
        decode_calls: Arc<Mutex<Vec<usize>>>,
    }

    #[cfg(feature = "cyrus-sasl")]
    impl SaslSecurityLayer for DummySecurityLayer {
        fn max_outbuf(&self) -> u32 {
            self.max
        }

        fn encode(&mut self, input: &[u8]) -> Result<Vec<u8>, SvnError> {
            self.encode_calls.lock().unwrap().push(input.len());
            Ok(invert_bytes(input))
        }

        fn decode(&mut self, input: &[u8]) -> Result<Vec<u8>, SvnError> {
            self.decode_calls.lock().unwrap().push(input.len());
            Ok(invert_bytes(input))
        }
    }

    #[test]
    fn select_mech_prefers_plain_over_anonymous() {
        run_async(async {
            let (conn, _server) =
                connected_conn(Some("alice".to_string()), Some("secret".to_string())).await;
            let mechs = vec!["ANONYMOUS".to_string(), "PLAIN".to_string()];
            let (mech, token) = conn.select_mech(&mechs).unwrap();
            assert_eq!(mech, "PLAIN");
            let mut expected = Vec::new();
            expected.push(0);
            expected.extend_from_slice(b"alice");
            expected.push(0);
            expected.extend_from_slice(b"secret");
            assert_eq!(token.unwrap(), expected);
        });
    }

    #[test]
    fn select_mech_uses_cram_md5_when_plain_missing() {
        run_async(async {
            let (conn, _server) =
                connected_conn(Some("alice".to_string()), Some("secret".to_string())).await;
            let mechs = vec!["CRAM-MD5".to_string(), "ANONYMOUS".to_string()];
            let (mech, token) = conn.select_mech(&mechs).unwrap();
            assert_eq!(mech, "CRAM-MD5");
            assert!(token.is_none());
        });
    }

    #[test]
    fn select_mech_falls_back_to_anonymous_without_creds() {
        run_async(async {
            let (conn, _server) = connected_conn(None, None).await;
            let mechs = vec!["ANONYMOUS".to_string()];
            let (mech, token) = conn.select_mech(&mechs).unwrap();
            assert_eq!(mech, "ANONYMOUS");
            assert_eq!(token.unwrap(), Vec::<u8>::new());
        });
    }

    #[test]
    fn select_mech_prefers_external_when_tunneled() {
        run_async(async {
            let (conn, _server) = connected_conn_tunneled(None, None).await;
            let mechs = vec!["ANONYMOUS".to_string(), "EXTERNAL".to_string()];
            let (mech, token) = conn.select_mech(&mechs).unwrap();
            assert_eq!(mech, "EXTERNAL");
            assert_eq!(token.unwrap(), Vec::<u8>::new());
        });
    }

    #[test]
    fn select_mech_reports_unavailable_when_no_supported_mechs() {
        run_async(async {
            let (conn, _server) = connected_conn(None, None).await;
            let mechs = vec!["PLAIN".to_string(), "CRAM-MD5".to_string()];
            let err = conn.select_mech(&mechs).unwrap_err();
            assert!(matches!(err, SvnError::AuthUnavailable));
        });
    }

    #[test]
    fn auth_request_retries_with_next_mechanism_on_failure() {
        run_async(async {
            let (mut conn, mut server) =
                connected_conn(Some("alice".to_string()), Some("secret".to_string())).await;

            let server_task = tokio::spawn(async move {
                let auth_request = SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::List(vec![
                            SvnItem::Word("CRAM-MD5".to_string()),
                            SvnItem::Word("PLAIN".to_string()),
                        ]),
                        SvnItem::String(b"realm".to_vec()),
                    ]),
                ]);
                write_item_line(&mut server, &auth_request).await;

                let first_response = read_until_newline(&mut server).await;
                let expected_first = SvnItem::List(vec![
                    SvnItem::Word("CRAM-MD5".to_string()),
                    SvnItem::List(Vec::new()),
                ]);
                let mut expected_first_bytes = Vec::new();
                encode_item(&expected_first, &mut expected_first_bytes);
                expected_first_bytes.push(b'\n');
                assert_eq!(first_response, expected_first_bytes);

                let failure = SvnItem::List(vec![
                    SvnItem::Word("failure".to_string()),
                    SvnItem::List(vec![SvnItem::String(b"bad".to_vec())]),
                ]);
                write_item_line(&mut server, &failure).await;

                let second_response = read_until_newline(&mut server).await;
                let mut token = Vec::new();
                token.push(0);
                token.extend_from_slice(b"alice");
                token.push(0);
                token.extend_from_slice(b"secret");
                let expected_second = SvnItem::List(vec![
                    SvnItem::Word("PLAIN".to_string()),
                    SvnItem::List(vec![SvnItem::String(token)]),
                ]);
                let mut expected_second_bytes = Vec::new();
                encode_item(&expected_second, &mut expected_second_bytes);
                expected_second_bytes.push(b'\n');
                assert_eq!(second_response, expected_second_bytes);

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![SvnItem::Word("success".to_string())]),
                )
                .await;
            });

            conn.handle_auth_request().await.unwrap();
            server_task.await.unwrap();
        });
    }

    #[test]
    fn auth_step_reply_cram_md5_matches_known_vector() {
        run_async(async {
            let (conn, _server) =
                connected_conn(Some("alice".to_string()), Some("key".to_string())).await;
            let reply = conn
                .auth_step_reply(
                    "CRAM-MD5",
                    b"The quick brown fox jumps over the lazy dog".to_vec(),
                )
                .unwrap();
            let reply = String::from_utf8(reply).unwrap();
            assert_eq!(reply, "alice 80070713463e7749b90c2dc24911e275");
        });
    }

    #[test]
    fn auth_step_reply_cram_md5_requires_creds() {
        run_async(async {
            let (conn, _server) = connected_conn(None, Some("key".to_string())).await;
            let err = conn
                .auth_step_reply("CRAM-MD5", b"challenge".to_vec())
                .unwrap_err();
            assert!(matches!(err, SvnError::AuthFailed(_)));

            let (conn, _server) = connected_conn(Some("alice".to_string()), None).await;
            let err = conn
                .auth_step_reply("CRAM-MD5", b"challenge".to_vec())
                .unwrap_err();
            assert!(matches!(err, SvnError::AuthFailed(_)));
        });
    }

    #[test]
    fn read_item_roundtrips_encoded_values() {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;
            let item = SvnItem::List(vec![
                SvnItem::Word("word".to_string()),
                SvnItem::Number(22),
                SvnItem::Bool(true),
                SvnItem::String(b"bytes".to_vec()),
                SvnItem::List(vec![SvnItem::Word("nested".to_string())]),
            ]);
            write_item_line(&mut server, &item).await;
            let parsed = conn.read_item().await.unwrap();
            assert_eq!(parsed, item);
        });
    }

    #[test]
    fn read_item_rejects_invalid_word_tokens() {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;
            server.write_all(b"wo(rd ").await.unwrap();
            server.flush().await.unwrap();
            let err = conn.read_item().await.unwrap_err();
            assert!(matches!(err, SvnError::Protocol(_)));
        });
    }

    #[test]
    fn read_item_rejects_number_overflow() {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;
            server.write_all(b"18446744073709551616 \n").await.unwrap();
            server.flush().await.unwrap();
            let err = conn.read_item().await.unwrap_err();
            assert!(matches!(err, SvnError::Protocol(_)));
        });
    }

    #[test]
    fn read_item_requires_whitespace_after_strings() {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;
            server.write_all(b"4:testX \n").await.unwrap();
            server.flush().await.unwrap();
            let err = conn.read_item().await.unwrap_err();
            assert!(matches!(err, SvnError::Protocol(msg) if msg == "expected whitespace"));
        });
    }

    #[test]
    fn handshake_writes_expected_client_greeting() {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;

            let server_task = tokio::spawn(async move {
                let greeting = SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::Number(2),
                        SvnItem::Number(2),
                        SvnItem::List(Vec::new()),
                        SvnItem::List(vec![
                            SvnItem::Word("edit-pipeline".to_string()),
                            SvnItem::Word("svndiff1".to_string()),
                        ]),
                    ]),
                ]);
                write_item_line(&mut server, &greeting).await;

                let client_greeting = read_until_newline(&mut server).await;
                let expected = SvnItem::List(vec![
                    SvnItem::Number(2),
                    SvnItem::List(vec![
                        SvnItem::Word("edit-pipeline".to_string()),
                        SvnItem::Word("svndiff1".to_string()),
                        SvnItem::Word("accepts-svndiff2".to_string()),
                        SvnItem::Word("absent-entries".to_string()),
                        SvnItem::Word("depth".to_string()),
                        SvnItem::Word("mergeinfo".to_string()),
                        SvnItem::Word("log-revprops".to_string()),
                    ]),
                    SvnItem::String(b"svn://example.com:3690/repo".to_vec()),
                    SvnItem::String(b"test-ra_svn".to_vec()),
                    SvnItem::List(Vec::new()),
                ]);
                let mut expected_bytes = Vec::new();
                encode_item(&expected, &mut expected_bytes);
                expected_bytes.push(b'\n');
                assert_eq!(client_greeting, expected_bytes);

                let auth_request = SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::List(Vec::new()),
                        SvnItem::String(b"realm".to_vec()),
                    ]),
                ]);
                write_item_line(&mut server, &auth_request).await;

                let repos_info = SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(b"uuid".to_vec()),
                        SvnItem::String(b"svn://example.com/repo".to_vec()),
                        SvnItem::List(vec![SvnItem::Word("mergeinfo".to_string())]),
                    ]),
                ]);
                write_item_line(&mut server, &repos_info).await;
            });

            let info = conn.handshake().await.unwrap();
            assert!(conn.server_has_cap("edit-pipeline"));
            assert!(conn.server_has_cap("svndiff1"));
            assert_eq!(info.repository.uuid, "uuid");
            assert_eq!(info.repository.root_url, "svn://example.com/repo");
            assert!(
                info.repository
                    .capabilities
                    .iter()
                    .any(|c| c == "mergeinfo")
            );
            server_task.await.unwrap();
        });
    }

    #[test]
    fn handshake_skips_leading_garbage_for_tunneled_connections() {
        run_async(async {
            let (mut conn, mut server) = connected_conn_tunneled(None, None).await;

            let server_task = tokio::spawn(async move {
                server
                    .write_all(b"Last login: Thu Jan 01 00:00:00 1970\\n")
                    .await
                    .unwrap();
                server.flush().await.unwrap();

                let greeting = SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::Number(2),
                        SvnItem::Number(2),
                        SvnItem::List(Vec::new()),
                        SvnItem::List(vec![
                            SvnItem::Word("edit-pipeline".to_string()),
                            SvnItem::Word("svndiff1".to_string()),
                        ]),
                    ]),
                ]);
                write_item_line(&mut server, &greeting).await;

                let client_greeting = read_until_newline(&mut server).await;
                let expected = SvnItem::List(vec![
                    SvnItem::Number(2),
                    SvnItem::List(vec![
                        SvnItem::Word("edit-pipeline".to_string()),
                        SvnItem::Word("svndiff1".to_string()),
                        SvnItem::Word("accepts-svndiff2".to_string()),
                        SvnItem::Word("absent-entries".to_string()),
                        SvnItem::Word("depth".to_string()),
                        SvnItem::Word("mergeinfo".to_string()),
                        SvnItem::Word("log-revprops".to_string()),
                    ]),
                    SvnItem::String(b"svn+ssh://example.com:22/repo".to_vec()),
                    SvnItem::String(b"test-ra_svn".to_vec()),
                    SvnItem::List(Vec::new()),
                ]);
                let mut expected_bytes = Vec::new();
                encode_item(&expected, &mut expected_bytes);
                expected_bytes.push(b'\n');
                assert_eq!(client_greeting, expected_bytes);

                let auth_request = SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::List(Vec::new()),
                        SvnItem::String(b"realm".to_vec()),
                    ]),
                ]);
                write_item_line(&mut server, &auth_request).await;

                let repos_info = SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(b"uuid".to_vec()),
                        SvnItem::String(b"svn://example.com/repo".to_vec()),
                        SvnItem::List(vec![SvnItem::Word("mergeinfo".to_string())]),
                    ]),
                ]);
                write_item_line(&mut server, &repos_info).await;
            });

            let info = conn.handshake().await.unwrap();
            assert_eq!(info.repository.uuid, "uuid");
            server_task.await.unwrap();
        });
    }

    #[test]
    fn handshake_rejects_servers_without_v2_support() {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;
            let server_task = tokio::spawn(async move {
                let greeting = SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::Number(3),
                        SvnItem::Number(4),
                        SvnItem::List(Vec::new()),
                        SvnItem::List(Vec::new()),
                    ]),
                ]);
                write_item_line(&mut server, &greeting).await;
            });

            let err = conn.handshake().await.unwrap_err();
            assert!(matches!(err, SvnError::Protocol(_)));
            server_task.await.unwrap();
        });
    }

    #[cfg(feature = "cyrus-sasl")]
    #[test]
    fn write_item_applies_security_layer_and_chunks() {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;

            let encode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
            let decode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
            conn.sasl = Some(Box::new(DummySecurityLayer {
                max: 8,
                encode_calls: encode_calls.clone(),
                decode_calls,
            }));

            let item = SvnItem::List(vec![
                SvnItem::Word("test".to_string()),
                SvnItem::String(vec![b'x'; 25]),
            ]);
            let mut plain = Vec::new();
            encode_item(&item, &mut plain);
            plain.push(b'\n');

            conn.write_item(&item).await.unwrap();

            let mut got = vec![0u8; plain.len()];
            server.read_exact(&mut got).await.unwrap();
            assert_eq!(got, invert_bytes(&plain));
            assert_eq!(*encode_calls.lock().unwrap(), chunk_sizes(plain.len(), 8));
        });
    }

    #[cfg(feature = "cyrus-sasl")]
    #[test]
    fn write_item_security_layer_skips_chunking_when_max_outbuf_zero() {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;

            let encode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
            let decode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
            conn.sasl = Some(Box::new(DummySecurityLayer {
                max: 0,
                encode_calls: encode_calls.clone(),
                decode_calls,
            }));

            let item = SvnItem::String(vec![b'a'; 10]);
            let mut plain = Vec::new();
            encode_item(&item, &mut plain);
            plain.push(b'\n');

            conn.write_item(&item).await.unwrap();

            let mut got = vec![0u8; plain.len()];
            server.read_exact(&mut got).await.unwrap();
            assert_eq!(got, invert_bytes(&plain));
            assert_eq!(*encode_calls.lock().unwrap(), vec![plain.len()]);
        });
    }

    #[cfg(feature = "cyrus-sasl")]
    #[test]
    fn read_item_decodes_with_security_layer() {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;

            let encode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
            let decode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
            conn.sasl = Some(Box::new(DummySecurityLayer {
                max: 0,
                encode_calls,
                decode_calls: decode_calls.clone(),
            }));

            let item = SvnItem::List(vec![
                SvnItem::Word("hello".to_string()),
                SvnItem::Number(1),
                SvnItem::String(b"world".to_vec()),
            ]);
            let mut plain = Vec::new();
            encode_item(&item, &mut plain);
            plain.push(b'\n');
            let wire = invert_bytes(&plain);
            server.write_all(&wire).await.unwrap();
            server.flush().await.unwrap();

            let parsed = conn.read_item().await.unwrap();
            assert_eq!(parsed, item);

            let decoded_total: usize = decode_calls.lock().unwrap().iter().sum();
            assert_eq!(decoded_total, wire.len());
        });
    }

    #[cfg(feature = "cyrus-sasl")]
    #[test]
    fn write_cmd_failure_early_applies_security_layer_and_chunks() {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;

            let encode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
            let decode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
            conn.sasl = Some(Box::new(DummySecurityLayer {
                max: 7,
                encode_calls: encode_calls.clone(),
                decode_calls,
            }));

            let err = SvnError::Protocol("boom".into());
            let done = conn.write_cmd_failure_early(&err).await.unwrap();
            assert!(!done);

            let item = SvnItem::List(vec![
                SvnItem::Word("failure".to_string()),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::Number(1),
                    SvnItem::String(err.to_string().into_bytes()),
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                ])]),
            ]);

            let mut plain = Vec::new();
            encode_item(&item, &mut plain);
            plain.push(b'\n');

            let mut got = vec![0u8; plain.len()];
            server.read_exact(&mut got).await.unwrap();
            assert_eq!(got, invert_bytes(&plain));
            assert_eq!(*encode_calls.lock().unwrap(), chunk_sizes(plain.len(), 7));
        });
    }
}
