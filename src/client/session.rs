use super::*;

impl RaSvnSession {
    /// Returns the [`RaSvnClient`] configuration used to create this session.
    pub fn client(&self) -> &RaSvnClient {
        &self.client
    }

    /// Returns server info collected during handshake, if connected.
    pub fn server_info(&self) -> Option<&ServerInfo> {
        self.server_info.as_ref()
    }

    /// Returns the repository UUID, if available.
    pub fn repos_uuid(&self) -> Option<&str> {
        self.server_info
            .as_ref()
            .map(|info| info.repository.uuid.as_str())
    }

    /// Returns the repository root URL, if available.
    ///
    /// Some older servers may not provide a root URL during handshake.
    pub fn repos_root_url(&self) -> Option<&str> {
        let root = self
            .server_info
            .as_ref()
            .map(|info| info.repository.root_url.as_str())?;
        if root.trim().is_empty() {
            None
        } else {
            Some(root)
        }
    }

    /// Returns `true` if the server advertised the given capability.
    pub fn has_capability(&self, capability: Capability) -> bool {
        let Some(info) = self.server_info.as_ref() else {
            return false;
        };
        let cap = capability.as_wire_word();
        info.server_caps.iter().any(|entry| entry == cap)
            || info
                .repository
                .capabilities
                .iter()
                .any(|entry| entry == cap)
    }

    /// Changes the repository URL for this session (server-side `reparent`).
    ///
    /// This is only allowed within the same `host:port` pair.
    pub async fn reparent(&mut self, new_base_url: SvnUrl) -> Result<(), SvnError> {
        if new_base_url.host != self.client.base_url.host
            || new_base_url.port != self.client.base_url.port
        {
            return Err(SvnError::InvalidUrl(
                "reparent requires same host and port".to_string(),
            ));
        }

        let new_url = new_base_url.url.clone();
        self.with_retry("reparent", move |conn| {
            let new_url = new_url.clone();
            Box::pin(async move {
                let response = conn
                    .call(
                        "reparent",
                        SvnItem::List(vec![SvnItem::String(new_url.as_bytes().to_vec())]),
                    )
                    .await?;
                let _ = response.success_params("reparent")?;
                conn.set_session_url(new_url);
                Ok(())
            })
        })
        .await?;

        self.client.base_url = new_base_url;
        Ok(())
    }

    /// Reconnects the underlying TCP connection and performs a new handshake.
    pub async fn reconnect(&mut self) -> Result<(), SvnError> {
        if !self.allow_reconnect {
            return Err(SvnError::Protocol(
                "reconnect not supported for this session".to_string(),
            ));
        }
        let mut attempt = 0usize;
        loop {
            match self.client.connect().await {
                Ok((conn, server_info)) => {
                    self.conn = Some(conn);
                    self.server_info = Some(server_info);
                    return Ok(());
                }
                Err(err) if attempt < self.client.reconnect_retries && is_retryable_error(&err) => {
                    attempt += 1;
                    debug!(
                        attempt,
                        max_retries = self.client.reconnect_retries,
                        error = %err,
                        "connect failed; reconnecting and retrying"
                    );
                }
                Err(err) => return Err(err),
            }
        }
    }

    pub(super) async fn ensure_connected(&mut self) -> Result<(), SvnError> {
        if self.conn.is_none() {
            self.reconnect().await?;
        }
        Ok(())
    }

    pub(super) async fn with_retry<T, F>(
        &mut self,
        op: &'static str,
        mut f: F,
    ) -> Result<T, SvnError>
    where
        F: for<'a> FnMut(
            &'a mut RaSvnConnection,
        ) -> Pin<Box<dyn Future<Output = Result<T, SvnError>> + Send + 'a>>,
        F: Send,
    {
        let mut attempt = 0usize;
        loop {
            self.ensure_connected().await?;
            let result = {
                let conn = self.conn_mut()?;
                f(conn).await
            };
            match result {
                Ok(value) => return Ok(value),
                Err(err)
                    if self.allow_reconnect
                        && is_retryable_error(&err)
                        && attempt < self.client.reconnect_retries =>
                {
                    attempt += 1;
                    debug!(
                        op,
                        attempt,
                        max_retries = self.client.reconnect_retries,
                        error = %err,
                        "connection lost; reconnecting and retrying"
                    );
                    self.conn = None;
                    self.reconnect().await?;
                }
                Err(err) => return Err(err),
            }
        }
    }

    pub(super) fn conn_mut(&mut self) -> Result<&mut RaSvnConnection, SvnError> {
        self.conn
            .as_mut()
            .ok_or_else(|| SvnError::Protocol("not connected".into()))
    }
}

pub(super) fn is_unknown_command_error(err: &SvnError) -> bool {
    match err {
        SvnError::Server(server) => server.is_unknown_command(),
        SvnError::Protocol(message) => {
            let message = message.to_ascii_lowercase();
            message.contains("unknown command") || message.contains("unknown cmd")
        }
        _ => false,
    }
}

pub(super) fn should_drop_connection(err: &SvnError) -> bool {
    matches!(err, SvnError::Protocol(_) | SvnError::Io(_))
}

pub(super) fn is_retryable_error(err: &SvnError) -> bool {
    match err {
        SvnError::Protocol(message) => message.contains("unexpected EOF"),
        SvnError::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}
