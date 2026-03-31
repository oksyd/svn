use super::*;

impl RaSvnSession {
    /// Runs `get-dir` and returns a directory listing.
    pub async fn list_dir(&mut self, path: &str, rev: Option<u64>) -> Result<DirListing, SvnError> {
        let path = validate_rel_dir_path(path)?;
        self.with_retry("get-dir", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };

                let fields = [
                    DirentField::Kind,
                    DirentField::Size,
                    DirentField::HasProps,
                    DirentField::CreatedRev,
                    DirentField::Time,
                    DirentField::LastAuthor,
                ];
                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    rev_tuple,
                    SvnItem::Bool(false), // want-props
                    SvnItem::Bool(true),  // want-contents
                    SvnItem::List(
                        fields
                            .iter()
                            .map(|f| SvnItem::Word(f.as_word().to_string()))
                            .collect(),
                    ),
                    SvnItem::Bool(false), // want-iprops (always false; use get-iprops)
                ]);

                let response = conn.call("get-dir", params).await?;
                let params = response.success_params("get-dir")?;
                parse_get_dir_listing(&path, params)
            })
        })
        .await
    }

    /// Runs `get-dir` and requests specific directory entry fields.
    pub async fn list_dir_with_fields(
        &mut self,
        path: &str,
        rev: Option<u64>,
        fields: &[DirentField],
    ) -> Result<DirListing, SvnError> {
        let path = validate_rel_dir_path(path)?;
        let fields = if fields.is_empty() {
            vec![DirentField::Kind]
        } else {
            fields.to_vec()
        };

        self.with_retry("get-dir", move |conn| {
            let path = path.clone();
            let fields = fields.clone();
            Box::pin(async move {
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };

                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    rev_tuple,
                    SvnItem::Bool(false), // want-props
                    SvnItem::Bool(true),  // want-contents
                    SvnItem::List(
                        fields
                            .iter()
                            .map(|f| SvnItem::Word(f.as_word().to_string()))
                            .collect(),
                    ),
                    SvnItem::Bool(false), // want-iprops
                ]);

                let response = conn.call("get-dir", params).await?;
                let params = response.success_params("get-dir")?;
                parse_get_dir_listing(&path, params)
            })
        })
        .await
    }

    /// Runs `check-path` and returns the node kind at `path` and `rev`.
    pub async fn check_path(&mut self, path: &str, rev: Option<u64>) -> Result<NodeKind, SvnError> {
        let path = validate_rel_dir_path(path)?;
        self.with_retry("check-path", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };

                let params =
                    SvnItem::List(vec![SvnItem::String(path.as_bytes().to_vec()), rev_tuple]);

                let response = conn.call("check-path", params).await?;
                let params = response.success_params("check-path")?;
                let kind_word = params
                    .first()
                    .and_then(opt_tuple_wordish)
                    .ok_or_else(|| SvnError::Protocol("check-path response missing kind".into()))?;
                Ok(NodeKind::from_word(&kind_word))
            })
        })
        .await
    }

    /// Runs `stat` and returns basic information about a node.
    ///
    /// Returns `Ok(None)` if the path does not exist.
    pub async fn stat(
        &mut self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Option<StatEntry>, SvnError> {
        let path = validate_rel_dir_path(path)?;
        let path_for_request = path.clone();
        let stat = self
            .with_retry("stat", move |conn| {
                let path = path_for_request.clone();
                Box::pin(async move {
                    let rev_tuple = match rev {
                        Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                        None => SvnItem::List(Vec::new()),
                    };

                    let params =
                        SvnItem::List(vec![SvnItem::String(path.as_bytes().to_vec()), rev_tuple]);

                    let response = conn.call("stat", params).await?;
                    let params = response.success_params("stat")?;
                    Ok(parse_stat_params(params))
                })
            })
            .await?;

        if stat.is_some() {
            return Ok(stat);
        }

        // Some svnserve versions include extra fields / nesting for `stat`. Fall back to
        // `check-path` so callers can still detect file/dir.
        let kind = self.check_path(&path, rev).await?;
        if kind == NodeKind::None {
            return Ok(None);
        }
        Ok(Some(StatEntry {
            kind,
            size: None,
            has_props: None,
            created_rev: None,
            created_date: None,
            last_author: None,
        }))
    }

    /// Runs [`RaSvnSession::list`] using a [`ListOptions`] builder.
    pub async fn list_with_options(
        &mut self,
        options: &ListOptions,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let patterns = if options.patterns.is_empty() {
            None
        } else {
            Some(options.patterns.as_slice())
        };
        self.list(
            &options.path,
            options.rev,
            options.depth,
            &options.fields,
            patterns,
        )
        .await
    }

    /// Runs [`RaSvnSession::list_each`] using a [`ListOptions`] builder.
    pub async fn list_with_options_each<F>(
        &mut self,
        options: &ListOptions,
        on_entry: F,
    ) -> Result<(), SvnError>
    where
        F: FnMut(DirEntry) -> Result<(), SvnError> + Send,
    {
        let patterns = if options.patterns.is_empty() {
            None
        } else {
            Some(options.patterns.as_slice())
        };
        self.list_each(
            &options.path,
            options.rev,
            options.depth,
            &options.fields,
            patterns,
            on_entry,
        )
        .await
    }

    /// Runs `list` (server capability), streaming directory entries to `on_entry`.
    ///
    /// This is a lower-allocation alternative to [`RaSvnSession::list`].
    ///
    /// Note: this method does not automatically retry on mid-stream connection loss.
    pub async fn list_each<F>(
        &mut self,
        path: &str,
        rev: Option<u64>,
        depth: Depth,
        fields: &[DirentField],
        patterns: Option<&[String]>,
        mut on_entry: F,
    ) -> Result<(), SvnError>
    where
        F: FnMut(DirEntry) -> Result<(), SvnError> + Send,
    {
        let path = validate_rel_dir_path(path)?;
        let fields = if fields.is_empty() {
            vec![DirentField::Kind]
        } else {
            fields.to_vec()
        };
        let patterns = patterns.map(ToOwned::to_owned);

        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let rev_tuple = match rev {
                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                None => SvnItem::List(Vec::new()),
            };

            let mut params_items = vec![
                SvnItem::String(path.as_bytes().to_vec()),
                rev_tuple,
                SvnItem::Word(depth.as_word().to_string()),
                SvnItem::List(
                    fields
                        .iter()
                        .map(|f| SvnItem::Word(f.as_word().to_string()))
                        .collect(),
                ),
            ];

            if let Some(patterns) = patterns.as_ref()
                && !patterns.is_empty()
            {
                params_items.push(SvnItem::List(
                    patterns
                        .iter()
                        .map(|p| SvnItem::String(p.as_bytes().to_vec()))
                        .collect(),
                ));
            }

            conn.send_command("list", SvnItem::List(params_items))
                .await?;
            conn.handle_auth_request().await?;

            loop {
                let item = conn.read_item().await?;
                match item {
                    SvnItem::Word(word) if word == "done" => break,
                    SvnItem::List(items) => on_entry(parse_list_dirent(items)?)?,
                    other => {
                        return Err(SvnError::Protocol(format!(
                            "unexpected list dirent item: {}",
                            other.kind()
                        )));
                    }
                }
            }

            let response = conn.read_command_response().await?;
            response.ensure_success("list")?;
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

    /// Runs `list` (server capability) and returns directory entries.
    pub async fn list(
        &mut self,
        path: &str,
        rev: Option<u64>,
        depth: Depth,
        fields: &[DirentField],
        patterns: Option<&[String]>,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let path = validate_rel_dir_path(path)?;
        let fields = if fields.is_empty() {
            vec![DirentField::Kind]
        } else {
            fields.to_vec()
        };
        let patterns = patterns.map(ToOwned::to_owned);

        self.with_retry("list", move |conn| {
            let path = path.clone();
            let fields = fields.clone();
            let patterns = patterns.clone();
            Box::pin(async move {
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };

                let mut params_items = vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    rev_tuple,
                    SvnItem::Word(depth.as_word().to_string()),
                    SvnItem::List(
                        fields
                            .iter()
                            .map(|f| SvnItem::Word(f.as_word().to_string()))
                            .collect(),
                    ),
                ];

                if let Some(patterns) = patterns.as_ref()
                    && !patterns.is_empty()
                {
                    params_items.push(SvnItem::List(
                        patterns
                            .iter()
                            .map(|p| SvnItem::String(p.as_bytes().to_vec()))
                            .collect(),
                    ));
                }

                conn.send_command("list", SvnItem::List(params_items))
                    .await?;
                conn.handle_auth_request().await?;

                let mut entries = Vec::new();
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(items) => entries.push(parse_list_dirent(items)?),
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected list dirent item: {}",
                                other.kind()
                            )));
                        }
                    }
                }

                let response = conn.read_command_response().await?;
                response.ensure_success("list")?;
                Ok(entries)
            })
        })
        .await
    }

    /// Recursively lists a directory.
    ///
    /// Uses the server `list` capability when available, otherwise falls back
    /// to repeated `get-dir` calls.
    pub async fn list_recursive(
        &mut self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let use_list = self
            .conn
            .as_ref()
            .map(|c| c.server_has_cap("list"))
            .unwrap_or(false);

        if use_list {
            return self
                .list(
                    path,
                    rev,
                    Depth::Infinity,
                    &[
                        DirentField::Kind,
                        DirentField::Size,
                        DirentField::HasProps,
                        DirentField::CreatedRev,
                        DirentField::Time,
                        DirentField::LastAuthor,
                    ],
                    None,
                )
                .await;
        }

        let mut out = Vec::new();
        let mut stack = vec![validate_rel_dir_path(path)?];
        while let Some(dir) = stack.pop() {
            let listing = self.list_dir(&dir, rev).await?;
            for entry in &listing.entries {
                if entry.kind == NodeKind::Dir {
                    stack.push(entry.path.clone());
                }
            }
            out.extend(listing.entries);
        }
        Ok(out)
    }
}
