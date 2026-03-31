use super::*;

pub(super) fn encode_proplist(props: &PropertyList) -> SvnItem {
    SvnItem::List(
        props
            .iter()
            .map(|(name, value)| {
                SvnItem::List(vec![
                    SvnItem::String(name.as_bytes().to_vec()),
                    SvnItem::String(value.clone()),
                ])
            })
            .collect(),
    )
}

fn txn_client_compat_version(ra_client: &str) -> String {
    if let Some(rest) = ra_client.strip_prefix("SVN/") {
        rest.split_whitespace().next().unwrap_or(rest).to_string()
    } else {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

pub(crate) struct CommitDrive<'a> {
    conn: &'a mut RaSvnConnection,
    batch: Vec<u8>,
    since_poll: usize,
    closed: bool,
}

impl<'a> CommitDrive<'a> {
    const MAX_BATCH_BYTES: usize = 256 * 1024;
    const MAX_COMMANDS_PER_BATCH: usize = 32;

    fn new(conn: &'a mut RaSvnConnection) -> Self {
        Self {
            conn,
            batch: Vec::new(),
            since_poll: 0,
            closed: false,
        }
    }

    pub(crate) async fn send(&mut self, command: &EditorCommand) -> Result<(), SvnError> {
        if self.closed {
            return Err(SvnError::Protocol(
                "commit editor already closed (close-edit sent)".into(),
            ));
        }
        if matches!(command, EditorCommand::AbortEdit) {
            return Err(SvnError::Protocol(
                "commit does not support user-supplied abort-edit".into(),
            ));
        }

        if self.since_poll == 0 {
            check_for_edit_status(self.conn).await?;
        }

        encode_editor_command(command, &mut self.batch)?;
        self.since_poll += 1;

        if self.since_poll >= Self::MAX_COMMANDS_PER_BATCH
            || self.batch.len() >= Self::MAX_BATCH_BYTES
        {
            self.flush().await?;
        }

        if matches!(command, EditorCommand::CloseEdit) {
            self.closed = true;
        }
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SvnError> {
        if !self.batch.is_empty() {
            self.conn.write_wire_bytes(&self.batch).await?;
            self.batch.clear();
        }
        self.since_poll = 0;
        Ok(())
    }

    async fn finish(&mut self) -> Result<CommitInfo, SvnError> {
        if !self.closed {
            return Err(SvnError::Protocol(
                "commit drive did not send close-edit".into(),
            ));
        }

        self.flush().await?;

        let response = self.conn.read_command_response().await?;
        response.ensure_success("commit")?;

        self.conn.handle_auth_request().await?;
        let item = self.conn.read_item().await?;
        parse_commit_info(&item)
    }
}

async fn check_for_edit_status(conn: &mut RaSvnConnection) -> Result<(), SvnError> {
    if !conn.data_available().await? {
        return Ok(());
    }

    conn.send_command("abort-edit", SvnItem::List(Vec::new()))
        .await?;
    let response = conn.read_command_response().await?;
    response.ensure_success("abort-edit")?;

    Err(SvnError::Protocol(
        "successful edit status returned too soon".into(),
    ))
}

impl RaSvnSession {
    /// Runs `commit` using a low-level editor command sequence.
    ///
    /// This is a low-level API: `commands` must form a valid edit and must end
    /// with [`EditorCommand::CloseEdit`].
    pub async fn commit(
        &mut self,
        options: &CommitOptions,
        commands: &[EditorCommand],
    ) -> Result<CommitInfo, SvnError> {
        if commands.is_empty() {
            return Err(SvnError::Protocol(
                "commit requires at least a close-edit command".into(),
            ));
        }
        if !matches!(commands.last(), Some(EditorCommand::CloseEdit)) {
            return Err(SvnError::Protocol(
                "commit editor commands must end with close-edit".into(),
            ));
        }
        if commands
            .iter()
            .take(commands.len().saturating_sub(1))
            .any(|c| matches!(c, EditorCommand::CloseEdit | EditorCommand::AbortEdit))
        {
            return Err(SvnError::Protocol(
                "commit editor commands may only close or abort at the end".into(),
            ));
        }
        if commands
            .iter()
            .any(|c| matches!(c, EditorCommand::AbortEdit))
        {
            return Err(SvnError::Protocol(
                "commit does not support user-supplied abort-edit".into(),
            ));
        }

        self.ensure_connected().await?;
        let result = async {
            let ra_client = self.client.ra_client.clone();
            let conn = self.conn_mut()?;
            let server_supports_revprops = conn.server_has_cap("commit-revprops");
            let server_supports_ephemeral_txnprops = conn.server_has_cap("ephemeral-txnprops");
            let has_non_log_revprops = options.rev_props.keys().any(|k| k != "svn:log");
            if !server_supports_revprops && has_non_log_revprops {
                return Err(SvnError::Protocol(
                    "server does not support setting revision properties during commit".into(),
                ));
            }

            let mut rev_props = options.rev_props.clone();
            rev_props.insert(
                "svn:log".to_string(),
                options.log_message.as_bytes().to_vec(),
            );
            if server_supports_revprops && server_supports_ephemeral_txnprops {
                let compat = txn_client_compat_version(&ra_client);
                rev_props.insert(
                    "svn:txn-client-compat-version".to_string(),
                    compat.as_bytes().to_vec(),
                );
                rev_props.insert(
                    "svn:txn-user-agent".to_string(),
                    ra_client.as_bytes().to_vec(),
                );
            }

            let mut lock_tokens_items = Vec::with_capacity(options.lock_tokens.len());
            for lock_token in &options.lock_tokens {
                let path = validate_rel_path(&lock_token.path)?;
                lock_tokens_items.push(SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    SvnItem::String(lock_token.token.as_bytes().to_vec()),
                ]));
            }

            let params = SvnItem::List(vec![
                SvnItem::String(options.log_message.as_bytes().to_vec()),
                SvnItem::List(lock_tokens_items),
                SvnItem::Bool(options.keep_locks),
                encode_proplist(&rev_props),
            ]);

            let response = conn.call("commit", params).await?;
            let _ = response.success_params("commit")?;

            const MAX_BATCH_BYTES: usize = 256 * 1024;
            const MAX_COMMANDS_PER_BATCH: usize = 32;

            let mut batch = Vec::new();
            let mut since_poll = 0usize;
            for command in commands {
                if since_poll == 0 {
                    check_for_edit_status(conn).await?;
                }
                encode_editor_command(command, &mut batch)?;
                since_poll += 1;
                if since_poll >= MAX_COMMANDS_PER_BATCH || batch.len() >= MAX_BATCH_BYTES {
                    conn.write_wire_bytes(&batch).await?;
                    batch.clear();
                    since_poll = 0;
                }
            }
            if !batch.is_empty() {
                conn.write_wire_bytes(&batch).await?;
            }

            let response = conn.read_command_response().await?;
            response.ensure_success("commit")?;

            conn.handle_auth_request().await?;
            let item = conn.read_item().await?;
            parse_commit_info(&item)
        }
        .await;
        if result.is_err() {
            self.conn = None;
        }
        result
    }

    pub(crate) async fn commit_drive<F>(
        &mut self,
        options: &CommitOptions,
        f: F,
    ) -> Result<CommitInfo, SvnError>
    where
        F: for<'d> FnOnce(
            &'d mut CommitDrive<'_>,
        ) -> Pin<Box<dyn Future<Output = Result<(), SvnError>> + 'd>>,
    {
        self.ensure_connected().await?;
        let result = async {
            let ra_client = self.client.ra_client.clone();
            let conn = self.conn_mut()?;
            let server_supports_revprops = conn.server_has_cap("commit-revprops");
            let server_supports_ephemeral_txnprops = conn.server_has_cap("ephemeral-txnprops");
            let has_non_log_revprops = options.rev_props.keys().any(|k| k != "svn:log");
            if !server_supports_revprops && has_non_log_revprops {
                return Err(SvnError::Protocol(
                    "server does not support setting revision properties during commit".into(),
                ));
            }

            let mut rev_props = options.rev_props.clone();
            rev_props.insert(
                "svn:log".to_string(),
                options.log_message.as_bytes().to_vec(),
            );
            if server_supports_revprops && server_supports_ephemeral_txnprops {
                let compat = txn_client_compat_version(&ra_client);
                rev_props.insert(
                    "svn:txn-client-compat-version".to_string(),
                    compat.as_bytes().to_vec(),
                );
                rev_props.insert(
                    "svn:txn-user-agent".to_string(),
                    ra_client.as_bytes().to_vec(),
                );
            }

            let mut lock_tokens_items = Vec::with_capacity(options.lock_tokens.len());
            for lock_token in &options.lock_tokens {
                let path = validate_rel_path(&lock_token.path)?;
                lock_tokens_items.push(SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    SvnItem::String(lock_token.token.as_bytes().to_vec()),
                ]));
            }

            let params = SvnItem::List(vec![
                SvnItem::String(options.log_message.as_bytes().to_vec()),
                SvnItem::List(lock_tokens_items),
                SvnItem::Bool(options.keep_locks),
                encode_proplist(&rev_props),
            ]);

            let response = conn.call("commit", params).await?;
            let _ = response.success_params("commit")?;

            let mut drive = CommitDrive::new(conn);
            {
                let fut = f(&mut drive);
                fut.await?;
            }
            drive.finish().await
        }
        .await;
        if result.is_err() {
            self.conn = None;
        }
        result
    }
}
