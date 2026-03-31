use crate::raw::SvnItem;
use crate::{AsyncEditorEventHandler, EditorEvent, EditorEventHandler, SvnError};

use super::super::conn::RaSvnConnection;
use super::decode::{parse_editor_event, parse_failure};

#[derive(Debug)]
pub(crate) enum EditorDriveStatus {
    Completed,
    Aborted(SvnError),
}

pub(crate) async fn drive_editor(
    conn: &mut RaSvnConnection,
    mut handler: Option<&mut dyn EditorEventHandler>,
    for_replay: bool,
) -> Result<EditorDriveStatus, SvnError> {
    loop {
        let (cmd, params_item) = read_command_item(conn).await?;
        let params = params_item.as_list().unwrap_or_default();
        if cmd == "failure" {
            return Err(parse_failure(&params));
        }

        match cmd.as_str() {
            "finish-replay" => {
                if !for_replay {
                    return Err(SvnError::Protocol(
                        "finish-replay is only valid during replay".into(),
                    ));
                }
                if let Some(handler) = handler.as_deref_mut()
                    && let Err(err) = handler.on_event(EditorEvent::FinishReplay)
                {
                    return handle_editor_consumer_error(conn, err, false).await;
                }
                return Ok(EditorDriveStatus::Completed);
            }
            "close-edit" => {
                if let Some(handler) = handler.as_deref_mut()
                    && let Err(err) = handler.on_event(EditorEvent::CloseEdit)
                {
                    return handle_editor_consumer_error(conn, err, false).await;
                }
                conn.write_cmd_success().await?;
                return Ok(EditorDriveStatus::Completed);
            }
            "abort-edit" => {
                if let Some(handler) = handler.as_deref_mut()
                    && let Err(err) = handler.on_event(EditorEvent::AbortEdit)
                {
                    return handle_editor_consumer_error(conn, err, false).await;
                }
                conn.write_cmd_success().await?;
                return Ok(EditorDriveStatus::Completed);
            }
            _ => {}
        }

        let event = match parse_editor_event(&cmd, &params) {
            Ok(event) => event,
            Err(err) => return handle_editor_consumer_error(conn, err, true).await,
        };
        if let Some(handler) = handler.as_deref_mut()
            && let Err(err) = handler.on_event(event)
        {
            return handle_editor_consumer_error(conn, err, true).await;
        }
    }
}

pub(crate) async fn drive_editor_async(
    conn: &mut RaSvnConnection,
    mut handler: Option<&mut dyn AsyncEditorEventHandler>,
    for_replay: bool,
) -> Result<EditorDriveStatus, SvnError> {
    loop {
        let (cmd, params_item) = read_command_item(conn).await?;
        let params = params_item.as_list().unwrap_or_default();
        if cmd == "failure" {
            return Err(parse_failure(&params));
        }

        match cmd.as_str() {
            "finish-replay" => {
                if !for_replay {
                    return Err(SvnError::Protocol(
                        "finish-replay is only valid during replay".into(),
                    ));
                }
                if let Some(handler) = handler.as_deref_mut()
                    && let Err(err) = handler.on_event(EditorEvent::FinishReplay).await
                {
                    return handle_editor_consumer_error(conn, err, false).await;
                }
                return Ok(EditorDriveStatus::Completed);
            }
            "close-edit" => {
                if let Some(handler) = handler.as_deref_mut()
                    && let Err(err) = handler.on_event(EditorEvent::CloseEdit).await
                {
                    return handle_editor_consumer_error(conn, err, false).await;
                }
                conn.write_cmd_success().await?;
                return Ok(EditorDriveStatus::Completed);
            }
            "abort-edit" => {
                if let Some(handler) = handler.as_deref_mut()
                    && let Err(err) = handler.on_event(EditorEvent::AbortEdit).await
                {
                    return handle_editor_consumer_error(conn, err, false).await;
                }
                conn.write_cmd_success().await?;
                return Ok(EditorDriveStatus::Completed);
            }
            _ => {}
        }

        let event = match parse_editor_event(&cmd, &params) {
            Ok(event) => event,
            Err(err) => return handle_editor_consumer_error(conn, err, true).await,
        };
        if let Some(handler) = handler.as_deref_mut()
            && let Err(err) = handler.on_event(event).await
        {
            return handle_editor_consumer_error(conn, err, true).await;
        }
    }
}

async fn handle_editor_consumer_error(
    conn: &mut RaSvnConnection,
    err: SvnError,
    drain: bool,
) -> Result<EditorDriveStatus, SvnError> {
    let done = conn.write_cmd_failure_early(&err).await?;
    if drain && !done {
        drain_until_abort_or_success(conn).await?;
    }
    Ok(EditorDriveStatus::Aborted(err))
}

async fn drain_until_abort_or_success(conn: &mut RaSvnConnection) -> Result<(), SvnError> {
    loop {
        let item = match conn.read_item().await {
            Ok(item) => item,
            Err(SvnError::Protocol(msg)) if msg == "unexpected EOF" => return Ok(()),
            Err(err) => return Err(err),
        };
        let SvnItem::List(parts) = item else {
            continue;
        };
        let Some(cmd) = parts.first().and_then(|item| item.as_word()) else {
            continue;
        };
        if cmd == "abort-edit" || cmd == "success" {
            return Ok(());
        }
    }
}

async fn read_command_item(conn: &mut RaSvnConnection) -> Result<(String, SvnItem), SvnError> {
    let item = conn.read_item().await?;
    let SvnItem::List(parts) = item else {
        return Err(SvnError::Protocol("expected command list".into()));
    };
    if parts.is_empty() {
        return Err(SvnError::Protocol("empty command list".into()));
    }
    let cmd = parts[0]
        .as_word()
        .ok_or_else(|| SvnError::Protocol("command name not a word".into()))?;
    let params = parts
        .get(1)
        .cloned()
        .unwrap_or_else(|| SvnItem::List(Vec::new()));
    Ok((cmd, params))
}
