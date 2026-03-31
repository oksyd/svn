use super::*;

impl RaSvnSession {
    /// Runs `update` using a client-provided report and consumes the editor drive.
    ///
    /// The report must end with [`ReportCommand::FinishReport`] or
    /// [`ReportCommand::AbortReport`]. Editor events are delivered to `handler`.
    pub async fn update(
        &mut self,
        options: &UpdateOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        require_finish_report(report)?;
        let target = validate_rel_dir_path(&options.target)?;
        let recurse = matches!(options.depth, Depth::Immediates | Depth::Infinity);

        self.ensure_connected().await?;
        let mut drop_conn = false;
        let result = async {
            let conn = self.conn_mut()?;
            let rev_tuple = match options.rev {
                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                None => SvnItem::List(Vec::new()),
            };
            let params = SvnItem::List(vec![
                rev_tuple,
                SvnItem::String(target.as_bytes().to_vec()),
                SvnItem::Bool(recurse),
                SvnItem::Word(options.depth.as_word().to_string()),
                SvnItem::Bool(options.send_copyfrom_args),
                SvnItem::Bool(options.ignore_ancestry),
            ]);
            conn.send_command("update", params).await?;
            conn.handle_auth_request().await?;
            send_report(conn, report).await?;
            conn.handle_auth_request().await?;
            let status = drive_editor(conn, Some(handler), false).await?;
            match status {
                EditorDriveStatus::Completed => {
                    let response = conn.read_command_response().await?;
                    let _ = response.success_params("update")?;
                    Ok(())
                }
                EditorDriveStatus::Aborted(err) => {
                    if let Err(resp_err) = conn.read_command_response().await {
                        drop_conn = true;
                        debug!(
                            error = %resp_err,
                            "failed to read command response after aborted editor drive"
                        );
                    }
                    Err(err)
                }
            }
        }
        .await;
        if drop_conn {
            self.conn = None;
        } else if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `update` using a client-provided report and consumes the editor drive with
    /// an async handler.
    ///
    /// The report must end with [`ReportCommand::FinishReport`] or
    /// [`ReportCommand::AbortReport`]. Editor events are delivered to `handler`.
    pub async fn update_with_async_handler(
        &mut self,
        options: &UpdateOptions,
        report: &Report,
        handler: &mut dyn AsyncEditorEventHandler,
    ) -> Result<(), SvnError> {
        require_finish_report(report)?;
        let target = validate_rel_dir_path(&options.target)?;
        let recurse = matches!(options.depth, Depth::Immediates | Depth::Infinity);

        self.ensure_connected().await?;
        let mut drop_conn = false;
        let result = async {
            let conn = self.conn_mut()?;
            let rev_tuple = match options.rev {
                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                None => SvnItem::List(Vec::new()),
            };
            let params = SvnItem::List(vec![
                rev_tuple,
                SvnItem::String(target.as_bytes().to_vec()),
                SvnItem::Bool(recurse),
                SvnItem::Word(options.depth.as_word().to_string()),
                SvnItem::Bool(options.send_copyfrom_args),
                SvnItem::Bool(options.ignore_ancestry),
            ]);
            conn.send_command("update", params).await?;
            conn.handle_auth_request().await?;
            send_report(conn, report).await?;
            conn.handle_auth_request().await?;
            let status = drive_editor_async(conn, Some(handler), false).await?;
            match status {
                EditorDriveStatus::Completed => {
                    let response = conn.read_command_response().await?;
                    let _ = response.success_params("update")?;
                    Ok(())
                }
                EditorDriveStatus::Aborted(err) => {
                    if let Err(resp_err) = conn.read_command_response().await {
                        drop_conn = true;
                        debug!(
                            error = %resp_err,
                            "failed to read command response after aborted editor drive"
                        );
                    }
                    Err(err)
                }
            }
        }
        .await;
        if drop_conn {
            self.conn = None;
        } else if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }
}
