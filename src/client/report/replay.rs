use super::*;

impl RaSvnSession {
    /// Runs `replay` and consumes the editor drive.
    pub async fn replay(
        &mut self,
        options: &ReplayOptions,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        self.ensure_connected().await?;
        let mut drop_conn = false;
        let result = async {
            let conn = self.conn_mut()?;
            let params = SvnItem::List(vec![
                SvnItem::Number(options.revision),
                SvnItem::Number(options.low_water_mark),
                SvnItem::Bool(options.send_deltas),
            ]);
            conn.send_command("replay", params).await?;
            conn.handle_auth_request().await?;
            let status = drive_editor(conn, Some(handler), true).await?;
            match status {
                EditorDriveStatus::Completed => {
                    let response = conn.read_command_response().await?;
                    let _ = response.success_params("replay")?;
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

    /// Runs `replay` for a single revision and emits editor events to `handler` (async).
    pub async fn replay_with_async_handler(
        &mut self,
        options: &ReplayOptions,
        handler: &mut dyn AsyncEditorEventHandler,
    ) -> Result<(), SvnError> {
        self.ensure_connected().await?;
        let mut drop_conn = false;
        let result = async {
            let conn = self.conn_mut()?;
            let params = SvnItem::List(vec![
                SvnItem::Number(options.revision),
                SvnItem::Number(options.low_water_mark),
                SvnItem::Bool(options.send_deltas),
            ]);
            conn.send_command("replay", params).await?;
            conn.handle_auth_request().await?;
            let status = drive_editor_async(conn, Some(handler), true).await?;
            match status {
                EditorDriveStatus::Completed => {
                    let response = conn.read_command_response().await?;
                    let _ = response.success_params("replay")?;
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

    /// Runs `replay-range` and emits revprops and editor events to `handler`.
    pub async fn replay_range(
        &mut self,
        options: &ReplayRangeOptions,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        if options.end_rev < options.start_rev {
            return Err(SvnError::Protocol(
                "end_rev must be greater than or equal to start_rev".into(),
            ));
        }

        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let params = SvnItem::List(vec![
                SvnItem::Number(options.start_rev),
                SvnItem::Number(options.end_rev),
                SvnItem::Number(options.low_water_mark),
                SvnItem::Bool(options.send_deltas),
            ]);
            conn.send_command("replay-range", params).await?;
            conn.handle_auth_request().await?;

            for _rev in options.start_rev..=options.end_rev {
                let item = conn.read_item().await?;
                match parse_replay_range_item(item)? {
                    ReplayRangeItem::RevProps(props) => {
                        handler.on_event(EditorEvent::RevProps { props })?;
                    }
                    ReplayRangeItem::Failure(err) => return Err(err),
                }

                let status = drive_editor(conn, Some(handler), true).await?;
                if let EditorDriveStatus::Aborted(err) = status {
                    return Err(err);
                }
            }

            let response = conn.read_command_response().await?;
            let _ = response.success_params("replay-range")?;
            Ok(())
        }
        .await;
        if result.is_err() {
            self.conn = None;
        }
        result
    }

    /// Runs `replay-range` and emits revprops and editor events to `handler` (async).
    pub async fn replay_range_with_async_handler(
        &mut self,
        options: &ReplayRangeOptions,
        handler: &mut dyn AsyncEditorEventHandler,
    ) -> Result<(), SvnError> {
        if options.end_rev < options.start_rev {
            return Err(SvnError::Protocol(
                "end_rev must be greater than or equal to start_rev".into(),
            ));
        }

        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let params = SvnItem::List(vec![
                SvnItem::Number(options.start_rev),
                SvnItem::Number(options.end_rev),
                SvnItem::Number(options.low_water_mark),
                SvnItem::Bool(options.send_deltas),
            ]);
            conn.send_command("replay-range", params).await?;
            conn.handle_auth_request().await?;

            for _rev in options.start_rev..=options.end_rev {
                let item = conn.read_item().await?;
                match parse_replay_range_item(item)? {
                    ReplayRangeItem::RevProps(props) => {
                        handler.on_event(EditorEvent::RevProps { props }).await?;
                    }
                    ReplayRangeItem::Failure(err) => return Err(err),
                }

                let status = drive_editor_async(conn, Some(handler), true).await?;
                if let EditorDriveStatus::Aborted(err) = status {
                    return Err(err);
                }
            }

            let response = conn.read_command_response().await?;
            let _ = response.success_params("replay-range")?;
            Ok(())
        }
        .await;
        if result.is_err() {
            self.conn = None;
        }
        result
    }
}

enum ReplayRangeItem {
    RevProps(PropertyList),
    Failure(SvnError),
}

fn parse_replay_range_item(item: SvnItem) -> Result<ReplayRangeItem, SvnError> {
    let SvnItem::List(parts) = item else {
        return Err(SvnError::Protocol("expected revprops tuple".into()));
    };
    if parts.len() != 2 {
        return Err(SvnError::Protocol(
            "replay-range item must contain kind and payload".into(),
        ));
    }

    let word = parts[0]
        .as_word()
        .ok_or_else(|| SvnError::Protocol("revprops tuple word not a word".into()))?;
    match word.as_str() {
        "revprops" => Ok(ReplayRangeItem::RevProps(parse_proplist(&parts[1])?)),
        "failure" => {
            let errors = parts[1]
                .as_list()
                .ok_or_else(|| SvnError::Protocol("replay-range failure not a list".into()))?;
            Ok(ReplayRangeItem::Failure(parse_failure(&errors)))
        }
        other => Err(SvnError::Protocol(format!(
            "expected revprops, found '{other}'"
        ))),
    }
}
