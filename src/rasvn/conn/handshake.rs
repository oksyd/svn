use super::*;

impl RaSvnConnection {
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
}
