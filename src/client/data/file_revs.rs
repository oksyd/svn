use super::*;

impl RaSvnSession {
    /// Returns file revision metadata and svndiff chunks for `path`.
    pub async fn get_file_revs(
        &mut self,
        path: &str,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
        include_merged_revisions: bool,
    ) -> Result<Vec<crate::FileRev>, SvnError> {
        let path = validate_rel_path(path)?;
        self.with_retry("get-file-revs", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let start_tuple = match start_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };
                let end_tuple = match end_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };

                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    start_tuple,
                    end_tuple,
                    SvnItem::Bool(include_merged_revisions),
                ]);

                conn.send_command("get-file-revs", params).await?;
                conn.handle_auth_request().await?;

                let mut out = Vec::new();
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(_) => {
                            let mut rev_entry = parse_file_rev_entry(item)?;
                            loop {
                                let chunk = conn.read_item().await?;
                                let Some(bytes) = chunk.as_bytes_string() else {
                                    return Err(SvnError::Protocol(
                                        "file-rev delta chunk not a string".into(),
                                    ));
                                };
                                if bytes.is_empty() {
                                    break;
                                }
                                rev_entry.delta_chunks.push(bytes);
                            }
                            out.push(rev_entry);
                        }
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected file-rev entry item: {}",
                                other.kind()
                            )));
                        }
                    }
                }

                let response = conn.read_command_response().await?;
                response.ensure_success("get-file-revs")?;
                if out.is_empty() {
                    return Err(SvnError::Protocol(
                        "The get-file-revs command didn't return any revisions".into(),
                    ));
                }
                Ok(out)
            })
        })
        .await
    }

    /// Runs `get-file-revs`, streaming revisions to `on_rev`.
    ///
    /// This is a lower-allocation alternative to [`RaSvnSession::get_file_revs`].
    ///
    /// Note: this method does not automatically retry on mid-stream connection loss.
    pub async fn get_file_revs_each<F>(
        &mut self,
        path: &str,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
        include_merged_revisions: bool,
        mut on_rev: F,
    ) -> Result<(), SvnError>
    where
        F: FnMut(crate::FileRev) -> Result<(), SvnError> + Send,
    {
        let path = validate_rel_path(path)?;

        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;

            let start_tuple = match start_rev {
                Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                None => SvnItem::List(Vec::new()),
            };
            let end_tuple = match end_rev {
                Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                None => SvnItem::List(Vec::new()),
            };

            let params = SvnItem::List(vec![
                SvnItem::String(path.as_bytes().to_vec()),
                start_tuple,
                end_tuple,
                SvnItem::Bool(include_merged_revisions),
            ]);

            conn.send_command("get-file-revs", params).await?;
            conn.handle_auth_request().await?;

            let mut saw_any = false;
            loop {
                let item = conn.read_item().await?;
                match item {
                    SvnItem::Word(word) if word == "done" => break,
                    SvnItem::List(_) => {
                        saw_any = true;
                        let mut rev_entry = parse_file_rev_entry(item)?;
                        loop {
                            let chunk = conn.read_item().await?;
                            let Some(bytes) = chunk.as_bytes_string() else {
                                return Err(SvnError::Protocol(
                                    "file-rev delta chunk not a string".into(),
                                ));
                            };
                            if bytes.is_empty() {
                                break;
                            }
                            rev_entry.delta_chunks.push(bytes);
                        }
                        on_rev(rev_entry)?;
                    }
                    other => {
                        return Err(SvnError::Protocol(format!(
                            "unexpected file-rev entry item: {}",
                            other.kind()
                        )));
                    }
                }
            }

            let response = conn.read_command_response().await?;
            response.ensure_success("get-file-revs")?;
            if !saw_any {
                return Err(SvnError::Protocol(
                    "The get-file-revs command didn't return any revisions".into(),
                ));
            }
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `get-file-revs` and returns file revisions with materialized contents.
    ///
    /// The server may omit text deltas for revisions where the file contents did
    /// not change; in that case, this method reuses the last known contents.
    ///
    /// If a text delta cannot be applied (for example due to an unexpected base),
    /// this method falls back to fetching the full contents via `get-file` for
    /// that revision.
    pub async fn get_file_revs_with_contents(
        &mut self,
        path: &str,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
        include_merged_revisions: bool,
        max_bytes: u64,
    ) -> Result<Vec<crate::FileRevContents>, SvnError> {
        let file_revs = self
            .get_file_revs(path, start_rev, end_rev, include_merged_revisions)
            .await?;

        let mut current: Option<Vec<u8>> = None;
        let mut out = Vec::with_capacity(file_revs.len());

        for file_rev in file_revs {
            let contents = if file_rev.delta_chunks.is_empty() {
                match current.as_ref() {
                    Some(bytes) => bytes.clone(),
                    None => {
                        self.get_file_bytes(&file_rev.path, file_rev.rev, max_bytes)
                            .await?
                    }
                }
            } else {
                let base = current.as_deref().unwrap_or(&[]);
                let mut buf = LimitedVecWriter::new(max_bytes);
                match crate::apply_textdelta(base, file_rev.delta_chunks.iter(), &mut buf).await {
                    Ok(()) => buf.into_inner(),
                    Err(err) => {
                        debug!(
                            "failed to apply file-revs textdelta for {}@{}: {err}; falling back to get-file",
                            file_rev.path, file_rev.rev
                        );
                        self.get_file_bytes(&file_rev.path, file_rev.rev, max_bytes)
                            .await?
                    }
                }
            };

            current = Some(contents.clone());
            out.push(crate::FileRevContents { file_rev, contents });
        }

        Ok(out)
    }
}
