use super::*;

impl RaSvnSession {
    /// Runs `get-deleted-rev` for a path and returns the deletion revision (if any).
    pub async fn get_deleted_rev(
        &mut self,
        path: &str,
        peg_rev: u64,
        end_rev: u64,
    ) -> Result<Option<u64>, SvnError> {
        let path = validate_rel_path(path)?;
        self.with_retry("get-deleted-rev", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    SvnItem::Number(peg_rev),
                    SvnItem::Number(end_rev),
                ]);

                let response = conn.call("get-deleted-rev", params).await?;
                if response.is_failure() && response.failure_server_error().is_missing_revision() {
                    return Ok(None);
                }
                if response.is_failure() {
                    return Err(response.failure("get-deleted-rev"));
                }

                let params = response.success_params("get-deleted-rev")?;
                let deleted = params
                    .first()
                    .and_then(|i| i.as_u64())
                    .ok_or_else(|| SvnError::Protocol("missing deleted rev".into()))?;
                Ok(Some(deleted))
            })
        })
        .await
    }

    /// Runs `get-file`, streaming file contents into `out`.
    ///
    /// Returns the number of bytes written.
    pub async fn get_file<W: tokio::io::AsyncWrite + Unpin>(
        &mut self,
        path: &str,
        rev: u64,
        want_props: bool,
        out: &mut W,
        max_bytes: u64,
    ) -> Result<u64, SvnError> {
        Ok(self
            .get_file_with_result(path, rev, want_props, out, max_bytes)
            .await?
            .bytes_written)
    }

    /// Runs `get-file` and collects file contents into memory.
    ///
    /// This is a convenience wrapper around [`RaSvnSession::get_file`] for
    /// callers that don't need streaming.
    pub async fn get_file_bytes(
        &mut self,
        path: &str,
        rev: u64,
        max_bytes: u64,
    ) -> Result<Vec<u8>, SvnError> {
        let mut out = LimitedVecWriter::new(max_bytes);
        let _ = self.get_file(path, rev, false, &mut out, max_bytes).await?;
        Ok(out.into_inner())
    }

    /// Like [`RaSvnSession::get_file`], but also returns additional metadata.
    pub async fn get_file_with_result<W: tokio::io::AsyncWrite + Unpin>(
        &mut self,
        path: &str,
        rev: u64,
        want_props: bool,
        out: &mut W,
        max_bytes: u64,
    ) -> Result<GetFileResult, SvnError> {
        let options = GetFileOptions {
            rev,
            want_props,
            want_iprops: false,
            max_bytes,
        };
        self.get_file_with_options(path, &options, out).await
    }

    /// Runs `get-file` with a [`GetFileOptions`] builder.
    pub async fn get_file_with_options<W: tokio::io::AsyncWrite + Unpin>(
        &mut self,
        path: &str,
        options: &GetFileOptions,
        out: &mut W,
    ) -> Result<GetFileResult, SvnError> {
        let rev = options.rev;
        let want_props = options.want_props;
        let want_iprops = options.want_iprops;
        let max_bytes = options.max_bytes;

        let path = validate_rel_path(path)?;
        self.ensure_connected().await?;
        let mut attempt = 0usize;
        loop {
            let mut written = 0u64;
            let result = {
                let conn = self.conn_mut()?;

                let params = SvnItem::List(vec![
                    SvnItem::String(path.clone().into_bytes()),
                    SvnItem::List(vec![SvnItem::Number(rev)]),
                    SvnItem::Bool(want_props),
                    SvnItem::Bool(true),
                    // The standard client always sends want-iprops as false and
                    // uses a separate `get-iprops` request (see protocol notes).
                    SvnItem::Bool(false),
                ]);

                conn.send_command("get-file", params).await?;
                conn.handle_auth_request().await?;

                let response = conn.read_command_response().await?;
                let params = response.success_params("get-file")?;
                let meta = parse_get_file_response_params(params)?;

                loop {
                    let item = conn.read_item().await?;
                    let Some(chunk) = item.as_bytes_string() else {
                        return Err(SvnError::Protocol("expected file chunk string".into()));
                    };
                    if chunk.is_empty() {
                        break;
                    }

                    written = written.saturating_add(chunk.len() as u64);
                    if written > max_bytes {
                        return Err(SvnError::Protocol(format!(
                            "downloaded file exceeds limit {max_bytes}"
                        )));
                    }
                    out.write_all(&chunk).await?;
                }

                let post = conn.read_command_response().await?;
                if post.is_failure() {
                    return Err(post.failure("get-file"));
                }

                Ok(GetFileResult {
                    rev: meta.rev,
                    checksum: meta.checksum,
                    props: meta.props,
                    inherited_props: meta.inherited_props,
                    bytes_written: written,
                })
            };

            match result {
                Ok(mut result) => {
                    if want_iprops && self.has_capability(Capability::InheritedProps) {
                        result.inherited_props =
                            self.inherited_props(&path, Some(result.rev)).await?;
                    }
                    return Ok(result);
                }
                Err(err)
                    if self.allow_reconnect
                        && written == 0
                        && is_retryable_error(&err)
                        && attempt < self.client.reconnect_retries =>
                {
                    debug!("get-file connection lost before data; reconnecting and retrying");
                    self.reconnect().await?;
                    attempt += 1;
                }
                Err(err) => {
                    if should_drop_connection(&err) {
                        self.conn = None;
                    }
                    return Err(err);
                }
            }
        }
    }
}
