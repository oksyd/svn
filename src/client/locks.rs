use super::*;

impl RaSvnSession {
    /// Runs `get-lock` and returns the lock for `path`, if any.
    pub async fn get_lock(&mut self, path: &str) -> Result<Option<LockDesc>, SvnError> {
        let path = validate_rel_path(path)?;
        self.with_retry("get-lock", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let response = conn
                    .call(
                        "get-lock",
                        SvnItem::List(vec![SvnItem::String(path.into_bytes())]),
                    )
                    .await?;
                let params = response.success_params("get-lock")?;
                let Some(tuple) = params.first() else {
                    return Ok(None);
                };
                let list = tuple
                    .as_list()
                    .ok_or_else(|| SvnError::Protocol("get-lock tuple not a list".into()))?;
                let Some(lock_item) = list.first() else {
                    return Ok(None);
                };
                Ok(Some(parse_lockdesc(lock_item)?))
            })
        })
        .await
    }

    /// Runs `get-locks` and returns all locks under a directory.
    pub async fn get_locks(&mut self, path: &str, depth: Depth) -> Result<Vec<LockDesc>, SvnError> {
        let path = validate_rel_dir_path(path)?;
        self.with_retry("get-locks", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    SvnItem::List(vec![SvnItem::Word(depth.as_word().to_string())]),
                ]);
                let response = conn.call("get-locks", params).await?;
                let params = response.success_params("get-locks")?;
                let locks_list = params
                    .first()
                    .and_then(|i| i.as_list())
                    .ok_or_else(|| SvnError::Protocol("get-locks response not a list".into()))?;
                let mut out = Vec::new();
                for item in locks_list {
                    out.push(parse_lockdesc(&item)?);
                }
                Ok(out)
            })
        })
        .await
    }

    /// Runs `lock` to acquire a lock for a single path.
    pub async fn lock(&mut self, path: &str, options: &LockOptions) -> Result<LockDesc, SvnError> {
        let path = validate_rel_path(path)?;
        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;

            let comment_tuple = match &options.comment {
                Some(comment) => SvnItem::List(vec![SvnItem::String(comment.as_bytes().to_vec())]),
                None => SvnItem::List(Vec::new()),
            };
            let current_rev_tuple = match options.current_rev {
                Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                None => SvnItem::List(Vec::new()),
            };

            let params = SvnItem::List(vec![
                SvnItem::String(path.as_bytes().to_vec()),
                comment_tuple,
                SvnItem::Bool(options.steal_lock),
                current_rev_tuple,
            ]);

            let response = conn.call("lock", params).await?;
            let params = response.success_params("lock")?;
            let lock_item = params
                .first()
                .ok_or_else(|| SvnError::Protocol("lock response missing lockdesc".into()))?;
            parse_lockdesc(lock_item)
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `lock-many` and returns a per-target result vector.
    ///
    /// The outer `Result` represents transport/protocol failures; each inner
    /// `Result` corresponds to one target.
    pub async fn lock_many(
        &mut self,
        options: &LockManyOptions,
        targets: &[LockTarget],
    ) -> Result<Vec<Result<LockDesc, SvnError>>, SvnError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_connected().await?;
        let result = async {
            let maybe_out: Option<Vec<Result<LockDesc, SvnError>>> = {
                let conn = self.conn_mut()?;

                let comment_tuple = match &options.comment {
                    Some(comment) => {
                        SvnItem::List(vec![SvnItem::String(comment.as_bytes().to_vec())])
                    }
                    None => SvnItem::List(Vec::new()),
                };

                let mut targets_items = Vec::with_capacity(targets.len());
                for target in targets {
                    let path = validate_rel_path(&target.path)?;
                    let rev_tuple = match target.current_rev {
                        Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                        None => SvnItem::List(Vec::new()),
                    };
                    targets_items.push(SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        rev_tuple,
                    ]));
                }

                let params = SvnItem::List(vec![
                    comment_tuple,
                    SvnItem::Bool(options.steal_lock),
                    SvnItem::List(targets_items),
                ]);

                conn.send_command("lock-many", params).await?;
                conn.handle_auth_request().await?;

                let mut out: Vec<Result<LockDesc, SvnError>> = Vec::with_capacity(targets.len());
                let mut unsupported = false;
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(items) => {
                            let status =
                                items.first().and_then(|i| i.as_word()).ok_or_else(|| {
                                    SvnError::Protocol("lock-many status not a word".into())
                                })?;
                            let params =
                                items.get(1).and_then(|i| i.as_list()).ok_or_else(|| {
                                    SvnError::Protocol("lock-many params not a list".into())
                                })?;
                            match status.as_str() {
                                "success" => {
                                    let lock_item = params.first().ok_or_else(|| {
                                        SvnError::Protocol(
                                            "lock-many success missing lockdesc".into(),
                                        )
                                    })?;
                                    out.push(parse_lockdesc(lock_item));
                                }
                                "failure" => {
                                    let err = parse_failure(&params);
                                    if out.is_empty() && is_unknown_command_error(&err) {
                                        unsupported = true;
                                        break;
                                    }
                                    out.push(Err(err));
                                }
                                other => {
                                    return Err(SvnError::Protocol(format!(
                                        "unexpected lock-many status: {other}"
                                    )));
                                }
                            }
                        }
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected lock-many item: {}",
                                other.kind()
                            )));
                        }
                    }
                    if out.len() > targets.len() {
                        return Err(SvnError::Protocol(
                            "lock-many returned more results than targets".into(),
                        ));
                    }
                }

                if unsupported {
                    Ok::<Option<Vec<Result<LockDesc, SvnError>>>, SvnError>(None)
                } else {
                    let response = conn.read_command_response().await?;
                    response.ensure_success("lock-many")?;
                    if out.len() != targets.len() {
                        return Err(SvnError::Protocol(format!(
                            "lock-many returned {} results for {} targets",
                            out.len(),
                            targets.len()
                        )));
                    }

                    Ok(Some(out))
                }
            }?;

            if let Some(out) = maybe_out {
                return Ok(out);
            }

            let mut out: Vec<Result<LockDesc, SvnError>> = Vec::with_capacity(targets.len());
            for target in targets {
                let opts = LockOptions {
                    comment: options.comment.clone(),
                    steal_lock: options.steal_lock,
                    current_rev: target.current_rev,
                };
                out.push(self.lock(&target.path, &opts).await);
            }
            Ok(out)
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `unlock` to release a lock for a single path.
    pub async fn unlock(&mut self, path: &str, options: &UnlockOptions) -> Result<(), SvnError> {
        let path = validate_rel_path(path)?;
        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;

            let token_tuple = match &options.token {
                Some(token) => SvnItem::List(vec![SvnItem::String(token.as_bytes().to_vec())]),
                None => SvnItem::List(Vec::new()),
            };

            let params = SvnItem::List(vec![
                SvnItem::String(path.as_bytes().to_vec()),
                token_tuple,
                SvnItem::Bool(options.break_lock),
            ]);

            let response = conn.call("unlock", params).await?;
            let _ = response.success_params("unlock")?;
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

    /// Runs `unlock-many` and returns a per-target result vector.
    ///
    /// The outer `Result` represents transport/protocol failures; each inner
    /// `Result` corresponds to one target.
    pub async fn unlock_many(
        &mut self,
        options: &UnlockManyOptions,
        targets: &[UnlockTarget],
    ) -> Result<Vec<Result<String, SvnError>>, SvnError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_connected().await?;
        let result = async {
            let maybe_out: Option<Vec<Result<String, SvnError>>> = {
                let conn = self.conn_mut()?;

                let mut targets_items = Vec::with_capacity(targets.len());
                for target in targets {
                    let path = validate_rel_path(&target.path)?;
                    let token_tuple = match &target.token {
                        Some(token) => {
                            SvnItem::List(vec![SvnItem::String(token.as_bytes().to_vec())])
                        }
                        None => SvnItem::List(Vec::new()),
                    };
                    targets_items.push(SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        token_tuple,
                    ]));
                }

                let params = SvnItem::List(vec![
                    SvnItem::Bool(options.break_lock),
                    SvnItem::List(targets_items),
                ]);

                conn.send_command("unlock-many", params).await?;
                conn.handle_auth_request().await?;

                let mut out: Vec<Result<String, SvnError>> = Vec::with_capacity(targets.len());
                let mut unsupported = false;
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(items) => {
                            let status =
                                items.first().and_then(|i| i.as_word()).ok_or_else(|| {
                                    SvnError::Protocol("unlock-many status not a word".into())
                                })?;
                            let params =
                                items.get(1).and_then(|i| i.as_list()).ok_or_else(|| {
                                    SvnError::Protocol("unlock-many params not a list".into())
                                })?;
                            match status.as_str() {
                                "success" => {
                                    let path = params
                                        .first()
                                        .and_then(|i| i.as_string())
                                        .ok_or_else(|| {
                                            SvnError::Protocol(
                                                "unlock-many success missing path".into(),
                                            )
                                        })?
                                        .trim_start_matches('/')
                                        .to_string();
                                    out.push(Ok(path));
                                }
                                "failure" => {
                                    let err = parse_failure(&params);
                                    if out.is_empty() && is_unknown_command_error(&err) {
                                        unsupported = true;
                                        break;
                                    }
                                    out.push(Err(err));
                                }
                                other => {
                                    return Err(SvnError::Protocol(format!(
                                        "unexpected unlock-many status: {other}"
                                    )));
                                }
                            }
                        }
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected unlock-many item: {}",
                                other.kind()
                            )));
                        }
                    }
                    if out.len() > targets.len() {
                        return Err(SvnError::Protocol(
                            "unlock-many returned more results than targets".into(),
                        ));
                    }
                }

                if unsupported {
                    Ok::<Option<Vec<Result<String, SvnError>>>, SvnError>(None)
                } else {
                    let response = conn.read_command_response().await?;
                    response.ensure_success("unlock-many")?;
                    if out.len() != targets.len() {
                        return Err(SvnError::Protocol(format!(
                            "unlock-many returned {} results for {} targets",
                            out.len(),
                            targets.len()
                        )));
                    }

                    Ok(Some(out))
                }
            }?;

            if let Some(out) = maybe_out {
                return Ok(out);
            }

            let mut out: Vec<Result<String, SvnError>> = Vec::with_capacity(targets.len());
            for target in targets {
                let path = validate_rel_path(&target.path)?;
                let opts = UnlockOptions {
                    token: target.token.clone(),
                    break_lock: options.break_lock,
                };
                match self.unlock(&path, &opts).await {
                    Ok(()) => out.push(Ok(path)),
                    Err(err) => out.push(Err(err)),
                }
            }
            Ok(out)
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }
}
