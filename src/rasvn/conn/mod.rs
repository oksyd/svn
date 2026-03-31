use super::SvnItem;
use super::encode_item;
use super::parse::{parse_repos_info, parse_server_error};
#[cfg(feature = "cyrus-sasl")]
use super::sasl::{CyrusSasl, SASL_CONTINUE, base64_decode, base64_encode};
use super::wire::encode_command_item;

use hmac::{Hmac, KeyInit, Mac};
use md5::Md5;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::Instant;
use tracing::debug;

use crate::{Capability, SvnError};

mod auth;
mod call;
mod handshake;
mod io;
#[cfg(test)]
mod tests;

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
}
