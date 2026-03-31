use super::*;

impl RaSvnSession {
    /// Convenience wrapper for [`RaSvnSession::log_with_options`] over a revision range.
    pub async fn log(&mut self, start_rev: u64, end_rev: u64) -> Result<Vec<LogEntry>, SvnError> {
        let options = LogOptions::between(start_rev, end_rev);
        self.log_with_options(&options).await
    }

    /// Runs `log` with a [`LogOptions`] builder.
    pub async fn log_with_options(
        &mut self,
        options: &LogOptions,
    ) -> Result<Vec<LogEntry>, SvnError> {
        let target_paths = options.target_paths.clone();
        let start_rev = options.start_rev;
        let end_rev = options.end_rev;
        let changed_paths = options.changed_paths;
        let strict_node = options.strict_node;
        let limit = options.limit;
        let include_merged_revisions = options.include_merged_revisions;
        let revprops = options.revprops.clone();

        self.with_retry("log", move |conn| {
            let target_paths = target_paths.clone();
            let revprops = revprops.clone();
            Box::pin(async move {
                let target_paths = SvnItem::List(
                    target_paths
                        .into_iter()
                        .map(|p| SvnItem::String(p.into_bytes()))
                        .collect(),
                );
                let start_rev_tuple = match start_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };
                let end_rev_tuple = match end_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };

                let (want_author, want_date, want_message, want_custom_revprops) = match &revprops {
                    LogRevProps::All => (true, true, true, true),
                    LogRevProps::Custom(names) => {
                        let mut want_author = false;
                        let mut want_date = false;
                        let mut want_message = false;
                        let mut want_custom_revprops = false;
                        for name in names {
                            match name.as_str() {
                                "svn:author" => want_author = true,
                                "svn:date" => want_date = true,
                                "svn:log" => want_message = true,
                                _ => want_custom_revprops = true,
                            }
                        }
                        (want_author, want_date, want_message, want_custom_revprops)
                    }
                };

                let mut params_items = vec![
                    target_paths,
                    start_rev_tuple,
                    end_rev_tuple,
                    SvnItem::Bool(changed_paths),
                    SvnItem::Bool(strict_node),
                    SvnItem::Number(limit),
                    SvnItem::Bool(include_merged_revisions),
                ];
                match &revprops {
                    LogRevProps::All => {
                        params_items.push(SvnItem::Word("all-revprops".to_string()));
                    }
                    LogRevProps::Custom(revprops) => {
                        params_items.push(SvnItem::Word("revprops".to_string()));
                        params_items.push(SvnItem::List(
                            revprops
                                .iter()
                                .map(|p| SvnItem::String(p.as_bytes().to_vec()))
                                .collect(),
                        ));
                    }
                }

                let params = SvnItem::List(params_items);

                conn.send_command("log", params).await?;
                conn.handle_auth_request().await?;

                let mut entries = Vec::new();
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(items) => {
                            let mut entry = parse_log_entry(items, want_custom_revprops)?;
                            if want_author && let Some(author) = entry.author.as_deref() {
                                entry
                                    .rev_props
                                    .insert("svn:author".to_string(), author.as_bytes().to_vec());
                            }
                            if want_date && let Some(date) = entry.date.as_deref() {
                                entry
                                    .rev_props
                                    .insert("svn:date".to_string(), date.as_bytes().to_vec());
                            }
                            if want_message && let Some(message) = entry.message.as_deref() {
                                entry
                                    .rev_props
                                    .insert("svn:log".to_string(), message.as_bytes().to_vec());
                            }
                            entries.push(entry);
                        }
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected log entry item: {}",
                                other.kind()
                            )));
                        }
                    }
                }

                let response = conn.read_command_response().await?;
                response.ensure_success("log")?;
                Ok(entries)
            })
        })
        .await
    }

    /// Runs `log` with a [`LogOptions`] builder, streaming entries to `on_entry`.
    ///
    /// This is a lower-allocation alternative to [`RaSvnSession::log_with_options`].
    ///
    /// Note: this method does not automatically retry on mid-stream connection loss.
    pub async fn log_each<F>(
        &mut self,
        options: &LogOptions,
        mut on_entry: F,
    ) -> Result<(), SvnError>
    where
        F: FnMut(LogEntry) -> Result<(), SvnError> + Send,
    {
        let target_paths = options.target_paths.clone();
        let start_rev = options.start_rev;
        let end_rev = options.end_rev;
        let changed_paths = options.changed_paths;
        let strict_node = options.strict_node;
        let limit = options.limit;
        let include_merged_revisions = options.include_merged_revisions;
        let revprops = options.revprops.clone();

        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;

            let target_paths = SvnItem::List(
                target_paths
                    .into_iter()
                    .map(|p| SvnItem::String(p.into_bytes()))
                    .collect(),
            );
            let start_rev_tuple = match start_rev {
                Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                None => SvnItem::List(Vec::new()),
            };
            let end_rev_tuple = match end_rev {
                Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                None => SvnItem::List(Vec::new()),
            };

            let (want_author, want_date, want_message, want_custom_revprops) = match &revprops {
                LogRevProps::All => (true, true, true, true),
                LogRevProps::Custom(names) => {
                    let mut want_author = false;
                    let mut want_date = false;
                    let mut want_message = false;
                    let mut want_custom_revprops = false;
                    for name in names {
                        match name.as_str() {
                            "svn:author" => want_author = true,
                            "svn:date" => want_date = true,
                            "svn:log" => want_message = true,
                            _ => want_custom_revprops = true,
                        }
                    }
                    (want_author, want_date, want_message, want_custom_revprops)
                }
            };

            let mut params_items = vec![
                target_paths,
                start_rev_tuple,
                end_rev_tuple,
                SvnItem::Bool(changed_paths),
                SvnItem::Bool(strict_node),
                SvnItem::Number(limit),
                SvnItem::Bool(include_merged_revisions),
            ];
            match &revprops {
                LogRevProps::All => {
                    params_items.push(SvnItem::Word("all-revprops".to_string()));
                }
                LogRevProps::Custom(revprops) => {
                    params_items.push(SvnItem::Word("revprops".to_string()));
                    params_items.push(SvnItem::List(
                        revprops
                            .iter()
                            .map(|p| SvnItem::String(p.as_bytes().to_vec()))
                            .collect(),
                    ));
                }
            }

            conn.send_command("log", SvnItem::List(params_items))
                .await?;
            conn.handle_auth_request().await?;

            loop {
                let item = conn.read_item().await?;
                match item {
                    SvnItem::Word(word) if word == "done" => break,
                    SvnItem::List(items) => {
                        let mut entry = parse_log_entry(items, want_custom_revprops)?;
                        if want_author && let Some(author) = entry.author.as_deref() {
                            entry
                                .rev_props
                                .insert("svn:author".to_string(), author.as_bytes().to_vec());
                        }
                        if want_date && let Some(date) = entry.date.as_deref() {
                            entry
                                .rev_props
                                .insert("svn:date".to_string(), date.as_bytes().to_vec());
                        }
                        if want_message && let Some(message) = entry.message.as_deref() {
                            entry
                                .rev_props
                                .insert("svn:log".to_string(), message.as_bytes().to_vec());
                        }
                        on_entry(entry)?;
                    }
                    other => {
                        return Err(SvnError::Protocol(format!(
                            "unexpected log entry item: {}",
                            other.kind()
                        )));
                    }
                }
            }

            let response = conn.read_command_response().await?;
            response.ensure_success("log")?;
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

    /// Runs `log` with a [`LogOptions`] builder, streaming entries to `on_entry`,
    /// with automatic retry on transient connection loss.
    ///
    /// When a retry happens, the `log` command is restarted. To avoid calling
    /// `on_entry` multiple times for the same revision, this method suppresses
    /// entries with a revision number that was already observed.
    pub async fn log_each_retrying<F>(
        &mut self,
        options: &LogOptions,
        mut on_entry: F,
    ) -> Result<(), SvnError>
    where
        F: FnMut(LogEntry) -> Result<(), SvnError> + Send,
    {
        let mut attempt = 0usize;
        let mut seen = std::collections::HashSet::<u64>::new();
        loop {
            let result = self
                .log_each(options, |entry| {
                    if seen.insert(entry.rev) {
                        on_entry(entry)
                    } else {
                        Ok(())
                    }
                })
                .await;

            match result {
                Ok(()) => return Ok(()),
                Err(err)
                    if self.allow_reconnect
                        && is_retryable_error(&err)
                        && attempt < self.client.reconnect_retries =>
                {
                    attempt += 1;
                    debug!(
                        attempt,
                        max_retries = self.client.reconnect_retries,
                        error = %err,
                        "log interrupted; reconnecting and resuming"
                    );
                    self.conn = None;
                }
                Err(err) => return Err(err),
            }
        }
    }
}
