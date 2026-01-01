use crate::path::{validate_rel_dir_path_ref, validate_rel_path, validate_rel_path_ref};
use crate::rasvn::parse::{parse_proplist, parse_server_error};
use crate::raw::SvnItem;
use crate::{
    AsyncEditorEventHandler, EditorCommand, EditorEvent, EditorEventHandler, Report, ReportCommand,
    SvnError,
};

use super::conn::RaSvnConnection;
use super::wire::WireEncoder;

pub(crate) async fn send_report(
    conn: &mut RaSvnConnection,
    report: &Report,
) -> Result<(), SvnError> {
    const MAX_BATCH_BYTES: usize = 64 * 1024;

    let mut buf = Vec::new();
    for command in &report.commands {
        let done = encode_report_command(command, &mut buf)?;
        if buf.len() >= MAX_BATCH_BYTES || done {
            conn.write_wire_bytes(&buf).await?;
            buf.clear();
        }
        if done {
            return Ok(());
        }
    }

    Err(SvnError::Protocol(
        "report did not end with finish-report/abort-report".into(),
    ))
}

fn encode_report_command(cmd: &ReportCommand, out: &mut Vec<u8>) -> Result<bool, SvnError> {
    let mut enc = WireEncoder::new(out);
    enc.list_start();
    match cmd {
        ReportCommand::SetPath {
            path,
            rev,
            start_empty,
            lock_token,
            depth,
        } => {
            let path = validate_rel_dir_path_ref(path)?;
            enc.word("set-path");
            enc.list_start();
            enc.string_str(path.as_ref());
            enc.number(*rev);
            enc.bool(*start_empty);
            enc.list_start();
            if let Some(token) = lock_token {
                enc.string_str(token);
            }
            enc.list_end();
            enc.word(depth.as_word());
            enc.list_end();
            enc.list_end();
            enc.newline();
            Ok(false)
        }
        ReportCommand::DeletePath { path } => {
            let path = validate_rel_path_ref(path)?;
            enc.word("delete-path");
            enc.list_start();
            enc.string_str(path.as_ref());
            enc.list_end();
            enc.list_end();
            enc.newline();
            Ok(false)
        }
        ReportCommand::LinkPath {
            path,
            url,
            rev,
            start_empty,
            lock_token,
            depth,
        } => {
            let path = validate_rel_dir_path_ref(path)?;
            enc.word("link-path");
            enc.list_start();
            enc.string_str(path.as_ref());
            enc.string_str(url);
            enc.number(*rev);
            enc.bool(*start_empty);
            enc.list_start();
            if let Some(token) = lock_token {
                enc.string_str(token);
            }
            enc.list_end();
            enc.word(depth.as_word());
            enc.list_end();
            enc.list_end();
            enc.newline();
            Ok(false)
        }
        ReportCommand::FinishReport => {
            enc.word("finish-report");
            enc.list_start();
            enc.list_end();
            enc.list_end();
            enc.newline();
            Ok(true)
        }
        ReportCommand::AbortReport => {
            enc.word("abort-report");
            enc.list_start();
            enc.list_end();
            enc.list_end();
            enc.newline();
            Ok(true)
        }
    }
}

#[cfg(test)]
pub(crate) async fn send_editor_command(
    conn: &mut RaSvnConnection,
    command: &EditorCommand,
) -> Result<(), SvnError> {
    let mut buf = Vec::new();
    encode_editor_command(command, &mut buf)?;
    conn.write_wire_bytes(&buf).await
}

pub(crate) fn encode_editor_command(
    cmd: &EditorCommand,
    out: &mut Vec<u8>,
) -> Result<(), SvnError> {
    let mut enc = WireEncoder::new(out);
    enc.list_start();
    match cmd {
        EditorCommand::OpenRoot { rev, token } => {
            enc.word("open-root");
            enc.list_start();
            enc.list_start();
            if let Some(rev) = rev {
                enc.number(*rev);
            }
            enc.list_end();
            enc.string_str(token);
            enc.list_end();
        }
        EditorCommand::DeleteEntry {
            path,
            rev,
            dir_token,
        } => {
            let path = validate_rel_path_ref(path)?;
            enc.word("delete-entry");
            enc.list_start();
            enc.string_str(path.as_ref());
            enc.list_start();
            enc.number(*rev);
            enc.list_end();
            enc.string_str(dir_token);
            enc.list_end();
        }
        EditorCommand::AddDir {
            path,
            parent_token,
            child_token,
            copy_from,
        } => {
            let path = validate_rel_path_ref(path)?;
            enc.word("add-dir");
            enc.list_start();
            enc.string_str(path.as_ref());
            enc.string_str(parent_token);
            enc.string_str(child_token);
            enc.list_start();
            if let Some((copy_path, copy_rev)) = copy_from {
                let copy_path = validate_rel_path_ref(copy_path)?;
                enc.string_str(copy_path.as_ref());
                enc.number(*copy_rev);
            }
            enc.list_end();
            enc.list_end();
        }
        EditorCommand::OpenDir {
            path,
            parent_token,
            child_token,
            rev,
        } => {
            let path = validate_rel_path_ref(path)?;
            enc.word("open-dir");
            enc.list_start();
            enc.string_str(path.as_ref());
            enc.string_str(parent_token);
            enc.string_str(child_token);
            enc.list_start();
            enc.number(*rev);
            enc.list_end();
            enc.list_end();
        }
        EditorCommand::ChangeDirProp {
            dir_token,
            name,
            value,
        } => {
            enc.word("change-dir-prop");
            enc.list_start();
            enc.string_str(dir_token);
            enc.string_str(name);
            enc.list_start();
            if let Some(value) = value {
                enc.string_bytes(value);
            }
            enc.list_end();
            enc.list_end();
        }
        EditorCommand::CloseDir { dir_token } => {
            enc.word("close-dir");
            enc.list_start();
            enc.string_str(dir_token);
            enc.list_end();
        }
        EditorCommand::AbsentDir { path, parent_token } => {
            let path = validate_rel_path_ref(path)?;
            enc.word("absent-dir");
            enc.list_start();
            enc.string_str(path.as_ref());
            enc.string_str(parent_token);
            enc.list_end();
        }
        EditorCommand::AddFile {
            path,
            dir_token,
            file_token,
            copy_from,
        } => {
            let path = validate_rel_path_ref(path)?;
            enc.word("add-file");
            enc.list_start();
            enc.string_str(path.as_ref());
            enc.string_str(dir_token);
            enc.string_str(file_token);
            enc.list_start();
            if let Some((copy_path, copy_rev)) = copy_from {
                let copy_path = validate_rel_path_ref(copy_path)?;
                enc.string_str(copy_path.as_ref());
                enc.number(*copy_rev);
            }
            enc.list_end();
            enc.list_end();
        }
        EditorCommand::OpenFile {
            path,
            dir_token,
            file_token,
            rev,
        } => {
            let path = validate_rel_path_ref(path)?;
            enc.word("open-file");
            enc.list_start();
            enc.string_str(path.as_ref());
            enc.string_str(dir_token);
            enc.string_str(file_token);
            enc.list_start();
            enc.number(*rev);
            enc.list_end();
            enc.list_end();
        }
        EditorCommand::ApplyTextDelta {
            file_token,
            base_checksum,
        } => {
            enc.word("apply-textdelta");
            enc.list_start();
            enc.string_str(file_token);
            enc.list_start();
            if let Some(checksum) = base_checksum {
                enc.string_str(checksum);
            }
            enc.list_end();
            enc.list_end();
        }
        EditorCommand::TextDeltaChunk { file_token, chunk } => {
            enc.word("textdelta-chunk");
            enc.list_start();
            enc.string_str(file_token);
            enc.string_bytes(chunk);
            enc.list_end();
        }
        EditorCommand::TextDeltaEnd { file_token } => {
            enc.word("textdelta-end");
            enc.list_start();
            enc.string_str(file_token);
            enc.list_end();
        }
        EditorCommand::ChangeFileProp {
            file_token,
            name,
            value,
        } => {
            enc.word("change-file-prop");
            enc.list_start();
            enc.string_str(file_token);
            enc.string_str(name);
            enc.list_start();
            if let Some(value) = value {
                enc.string_bytes(value);
            }
            enc.list_end();
            enc.list_end();
        }
        EditorCommand::CloseFile {
            file_token,
            text_checksum,
        } => {
            enc.word("close-file");
            enc.list_start();
            enc.string_str(file_token);
            enc.list_start();
            if let Some(checksum) = text_checksum {
                enc.string_str(checksum);
            }
            enc.list_end();
            enc.list_end();
        }
        EditorCommand::AbsentFile { path, parent_token } => {
            let path = validate_rel_path_ref(path)?;
            enc.word("absent-file");
            enc.list_start();
            enc.string_str(path.as_ref());
            enc.string_str(parent_token);
            enc.list_end();
        }
        EditorCommand::CloseEdit => {
            enc.word("close-edit");
            enc.list_start();
            enc.list_end();
        }
        EditorCommand::AbortEdit => {
            enc.word("abort-edit");
            enc.list_start();
            enc.list_end();
        }
    }
    enc.list_end();
    enc.newline();
    Ok(())
}

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
        let Some(cmd) = parts.first().and_then(|i| i.as_word()) else {
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

pub(crate) fn parse_failure(params: &[SvnItem]) -> SvnError {
    SvnError::Server(parse_server_error(params))
}

fn parse_editor_event(cmd: &str, params: &[SvnItem]) -> Result<EditorEvent, SvnError> {
    match cmd {
        "target-rev" => {
            let rev = params
                .first()
                .and_then(|i| i.as_u64())
                .ok_or_else(|| SvnError::Protocol("target-rev missing rev".into()))?;
            Ok(EditorEvent::TargetRev { rev })
        }
        "open-root" => {
            if params.len() < 2 {
                return Err(SvnError::Protocol("open-root params too short".into()));
            }
            let rev = opt_tuple_u64(&params[0]);
            let token = req_string(&params[1], "open-root token")?;
            Ok(EditorEvent::OpenRoot { rev, token })
        }
        "delete-entry" => {
            if params.len() < 3 {
                return Err(SvnError::Protocol("delete-entry params too short".into()));
            }
            let rev = opt_tuple_u64(&params[1])
                .ok_or_else(|| SvnError::Protocol("delete-entry missing rev".into()))?;
            Ok(EditorEvent::DeleteEntry {
                path: req_rel_path(&params[0], "delete-entry path")?,
                rev,
                dir_token: req_string(&params[2], "delete-entry dir token")?,
            })
        }
        "add-dir" => {
            if params.len() < 3 {
                return Err(SvnError::Protocol("add-dir params too short".into()));
            }
            Ok(EditorEvent::AddDir {
                path: req_rel_path(&params[0], "add-dir path")?,
                parent_token: req_string(&params[1], "add-dir parent token")?,
                child_token: req_string(&params[2], "add-dir child token")?,
                copy_from: match params.get(3) {
                    Some(item) => opt_tuple_copyfrom(item)?,
                    None => None,
                },
            })
        }
        "open-dir" => {
            if params.len() < 4 {
                return Err(SvnError::Protocol("open-dir params too short".into()));
            }
            let rev = opt_tuple_u64(&params[3])
                .ok_or_else(|| SvnError::Protocol("open-dir missing rev".into()))?;
            Ok(EditorEvent::OpenDir {
                path: req_rel_path(&params[0], "open-dir path")?,
                parent_token: req_string(&params[1], "open-dir parent token")?,
                child_token: req_string(&params[2], "open-dir child token")?,
                rev,
            })
        }
        "change-dir-prop" => {
            if params.len() < 2 {
                return Err(SvnError::Protocol(
                    "change-dir-prop params too short".into(),
                ));
            }
            Ok(EditorEvent::ChangeDirProp {
                dir_token: req_string(&params[0], "change-dir-prop token")?,
                name: req_string(&params[1], "change-dir-prop name")?,
                value: params.get(2).and_then(opt_tuple_bytes),
            })
        }
        "close-dir" => {
            let token = params
                .first()
                .and_then(|i| i.as_string())
                .ok_or_else(|| SvnError::Protocol("close-dir missing token".into()))?;
            Ok(EditorEvent::CloseDir { dir_token: token })
        }
        "absent-dir" => {
            if params.len() < 2 {
                return Err(SvnError::Protocol("absent-dir params too short".into()));
            }
            Ok(EditorEvent::AbsentDir {
                path: req_rel_path(&params[0], "absent-dir path")?,
                parent_token: req_string(&params[1], "absent-dir parent token")?,
            })
        }
        "add-file" => {
            if params.len() < 3 {
                return Err(SvnError::Protocol("add-file params too short".into()));
            }
            Ok(EditorEvent::AddFile {
                path: req_rel_path(&params[0], "add-file path")?,
                dir_token: req_string(&params[1], "add-file dir token")?,
                file_token: req_string(&params[2], "add-file file token")?,
                copy_from: match params.get(3) {
                    Some(item) => opt_tuple_copyfrom(item)?,
                    None => None,
                },
            })
        }
        "open-file" => {
            if params.len() < 4 {
                return Err(SvnError::Protocol("open-file params too short".into()));
            }
            let rev = opt_tuple_u64(&params[3])
                .ok_or_else(|| SvnError::Protocol("open-file missing rev".into()))?;
            Ok(EditorEvent::OpenFile {
                path: req_rel_path(&params[0], "open-file path")?,
                dir_token: req_string(&params[1], "open-file dir token")?,
                file_token: req_string(&params[2], "open-file file token")?,
                rev,
            })
        }
        "apply-textdelta" => {
            if params.is_empty() {
                return Err(SvnError::Protocol(
                    "apply-textdelta params too short".into(),
                ));
            }
            Ok(EditorEvent::ApplyTextDelta {
                file_token: req_string(&params[0], "apply-textdelta token")?,
                base_checksum: params.get(1).and_then(opt_tuple_string),
            })
        }
        "textdelta-chunk" => {
            if params.len() < 2 {
                return Err(SvnError::Protocol(
                    "textdelta-chunk params too short".into(),
                ));
            }
            Ok(EditorEvent::TextDeltaChunk {
                file_token: req_string(&params[0], "textdelta-chunk token")?,
                chunk: req_bytes(&params[1], "textdelta-chunk chunk")?,
            })
        }
        "textdelta-end" => {
            let token = params
                .first()
                .and_then(|i| i.as_string())
                .ok_or_else(|| SvnError::Protocol("textdelta-end missing token".into()))?;
            Ok(EditorEvent::TextDeltaEnd { file_token: token })
        }
        "change-file-prop" => {
            if params.len() < 2 {
                return Err(SvnError::Protocol(
                    "change-file-prop params too short".into(),
                ));
            }
            Ok(EditorEvent::ChangeFileProp {
                file_token: req_string(&params[0], "change-file-prop token")?,
                name: req_string(&params[1], "change-file-prop name")?,
                value: params.get(2).and_then(opt_tuple_bytes),
            })
        }
        "close-file" => {
            if params.is_empty() {
                return Err(SvnError::Protocol("close-file params too short".into()));
            }
            Ok(EditorEvent::CloseFile {
                file_token: req_string(&params[0], "close-file token")?,
                text_checksum: params.get(1).and_then(opt_tuple_string),
            })
        }
        "absent-file" => {
            if params.len() < 2 {
                return Err(SvnError::Protocol("absent-file params too short".into()));
            }
            Ok(EditorEvent::AbsentFile {
                path: req_rel_path(&params[0], "absent-file path")?,
                parent_token: req_string(&params[1], "absent-file parent token")?,
            })
        }
        "revprops" => {
            let props = parse_proplist(&SvnItem::List(params.to_vec()))?;
            Ok(EditorEvent::RevProps { props })
        }
        _ => Err(SvnError::Protocol(format!("unknown editor command: {cmd}"))),
    }
}

fn req_string(item: &SvnItem, ctx: &str) -> Result<String, SvnError> {
    item.as_string()
        .ok_or_else(|| SvnError::Protocol(format!("{ctx} not a string")))
}

fn req_rel_path(item: &SvnItem, ctx: &str) -> Result<String, SvnError> {
    let raw = req_string(item, ctx)?;
    validate_rel_path(&raw)
}

fn req_bytes(item: &SvnItem, ctx: &str) -> Result<Vec<u8>, SvnError> {
    item.as_bytes_string()
        .ok_or_else(|| SvnError::Protocol(format!("{ctx} not a string")))
}

fn opt_tuple_u64(item: &SvnItem) -> Option<u64> {
    match item {
        SvnItem::List(items) => items.first().and_then(|i| i.as_u64()),
        _ => item.as_u64(),
    }
}

fn opt_tuple_string(item: &SvnItem) -> Option<String> {
    match item {
        SvnItem::List(items) => items.first().and_then(|i| i.as_string()),
        _ => item.as_string(),
    }
}

fn opt_tuple_bytes(item: &SvnItem) -> Option<Vec<u8>> {
    match item {
        SvnItem::List(items) => items.first().and_then(|i| i.as_bytes_string()),
        _ => item.as_bytes_string(),
    }
}

fn opt_tuple_copyfrom(item: &SvnItem) -> Result<Option<(String, u64)>, SvnError> {
    let Some(items) = item.as_list() else {
        return Ok(None);
    };
    if items.is_empty() {
        return Ok(None);
    }
    if items.len() < 2 {
        return Err(SvnError::Protocol("copy-from tuple too short".into()));
    }
    let path = items[0]
        .as_string()
        .ok_or_else(|| SvnError::Protocol("copy-from path not a string".into()))?;
    let rev = items[1]
        .as_u64()
        .ok_or_else(|| SvnError::Protocol("copy-from rev not a number".into()))?;
    let path = validate_rel_path(&path)?;
    Ok(Some((path, rev)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::Depth;
    use crate::rasvn::conn::RaSvnConnectionConfig;
    use crate::rasvn::encode_item;
    use std::future::Future;
    use std::pin::Pin;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn run_async<T>(f: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    async fn connected_conn() -> (RaSvnConnection, tokio::net::TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept_task = tokio::spawn(async move { listener.accept().await });
        let client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (server, _) = accept_task.await.unwrap().unwrap();

        let (read, write) = client.into_split();
        let conn = RaSvnConnection::new(
            Box::new(read),
            Box::new(write),
            RaSvnConnectionConfig {
                username: None,
                password: None,
                #[cfg(feature = "cyrus-sasl")]
                host: "example.com".to_string(),
                #[cfg(feature = "cyrus-sasl")]
                local_addrport: None,
                #[cfg(feature = "cyrus-sasl")]
                remote_addrport: None,
                is_tunneled: false,
                url: "svn://example.com:3690/repo".to_string(),
                ra_client: "test-ra_svn".to_string(),
                read_timeout: Duration::from_secs(1),
                write_timeout: Duration::from_secs(1),
            },
        );
        (conn, server)
    }

    fn encode_line(item: &SvnItem) -> Vec<u8> {
        let mut buf = Vec::new();
        encode_item(item, &mut buf);
        buf.push(b'\n');
        buf
    }

    async fn write_item_line(stream: &mut tokio::net::TcpStream, item: &SvnItem) {
        let buf = encode_line(item);
        stream.write_all(&buf).await.unwrap();
        stream.flush().await.unwrap();
    }

    async fn read_line(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).await.unwrap();
            if n == 0 {
                break;
            }
            buf.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        buf
    }

    #[test]
    fn send_report_writes_expected_commands() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;
            let mut report = Report::new();
            report
                .push(ReportCommand::SetPath {
                    path: "trunk".to_string(),
                    rev: 10,
                    start_empty: true,
                    lock_token: None,
                    depth: Depth::Infinity,
                })
                .finish();

            send_report(&mut conn, &report).await.unwrap();

            let expected_set_path = SvnItem::List(vec![
                SvnItem::Word("set-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk".to_vec()),
                    SvnItem::Number(10),
                    SvnItem::Bool(true),
                    SvnItem::List(Vec::new()),
                    SvnItem::Word("infinity".to_string()),
                ]),
            ]);
            let expected_finish = SvnItem::List(vec![
                SvnItem::Word("finish-report".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_set_path)
            );
            assert_eq!(read_line(&mut server).await, encode_line(&expected_finish));
        });
    }

    #[test]
    fn send_report_normalizes_paths() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;
            let mut report = Report::new();
            report
                .push(ReportCommand::SetPath {
                    path: "//trunk\\\\sub//./".to_string(),
                    rev: 10,
                    start_empty: true,
                    lock_token: None,
                    depth: Depth::Infinity,
                })
                .finish();

            send_report(&mut conn, &report).await.unwrap();

            let expected_set_path = SvnItem::List(vec![
                SvnItem::Word("set-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/sub".to_vec()),
                    SvnItem::Number(10),
                    SvnItem::Bool(true),
                    SvnItem::List(Vec::new()),
                    SvnItem::Word("infinity".to_string()),
                ]),
            ]);
            let expected_finish = SvnItem::List(vec![
                SvnItem::Word("finish-report".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_set_path)
            );
            assert_eq!(read_line(&mut server).await, encode_line(&expected_finish));
        });
    }

    #[test]
    fn send_report_rejects_unsafe_paths() {
        run_async(async {
            let (mut conn, _server) = connected_conn().await;
            let mut report = Report::new();
            report
                .push(ReportCommand::SetPath {
                    path: "trunk/../x".to_string(),
                    rev: 1,
                    start_empty: true,
                    lock_token: None,
                    depth: Depth::Infinity,
                })
                .finish();

            let err = send_report(&mut conn, &report).await.unwrap_err();
            assert!(matches!(err, SvnError::InvalidPath(_)));
        });
    }

    #[test]
    fn send_report_requires_terminator() {
        run_async(async {
            let (mut conn, _server) = connected_conn().await;
            let report = Report {
                commands: vec![ReportCommand::DeletePath {
                    path: "trunk/file.txt".to_string(),
                }],
            };
            let err = send_report(&mut conn, &report).await.unwrap_err();
            assert!(matches!(err, SvnError::Protocol(_)));
        });
    }

    #[test]
    fn send_editor_command_encodes_revision_as_optional_tuple() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;

            let cmd = EditorCommand::DeleteEntry {
                path: "trunk/old.txt".to_string(),
                rev: 5,
                dir_token: "d".to_string(),
            };
            send_editor_command(&mut conn, &cmd).await.unwrap();

            let expected = SvnItem::List(vec![
                SvnItem::Word("delete-entry".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/old.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(5)]),
                    SvnItem::String(b"d".to_vec()),
                ]),
            ]);
            assert_eq!(read_line(&mut server).await, encode_line(&expected));

            let cmd = EditorCommand::OpenDir {
                path: "trunk".to_string(),
                parent_token: "r".to_string(),
                child_token: "t".to_string(),
                rev: 5,
            };
            send_editor_command(&mut conn, &cmd).await.unwrap();

            let expected = SvnItem::List(vec![
                SvnItem::Word("open-dir".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk".to_vec()),
                    SvnItem::String(b"r".to_vec()),
                    SvnItem::String(b"t".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(5)]),
                ]),
            ]);
            assert_eq!(read_line(&mut server).await, encode_line(&expected));

            let cmd = EditorCommand::OpenFile {
                path: "trunk/file.txt".to_string(),
                dir_token: "t".to_string(),
                file_token: "f".to_string(),
                rev: 5,
            };
            send_editor_command(&mut conn, &cmd).await.unwrap();

            let expected = SvnItem::List(vec![
                SvnItem::Word("open-file".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::String(b"t".to_vec()),
                    SvnItem::String(b"f".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(5)]),
                ]),
            ]);
            assert_eq!(read_line(&mut server).await, encode_line(&expected));
        });
    }

    #[test]
    fn drive_editor_sends_success_on_close_edit() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;

            struct Collector {
                events: Vec<EditorEvent>,
            }

            impl EditorEventHandler for Collector {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    self.events.push(event);
                    Ok(())
                }
            }

            let server_task = tokio::spawn(async move {
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("target-rev".to_string()),
                        SvnItem::List(vec![SvnItem::Number(42)]),
                    ]),
                )
                .await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("close-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;
                read_line(&mut server).await
            });

            let mut handler = Collector { events: Vec::new() };
            let status = drive_editor(&mut conn, Some(&mut handler), false)
                .await
                .unwrap();
            assert!(matches!(status, EditorDriveStatus::Completed));

            let response_line = server_task.await.unwrap();
            let expected_response = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);
            assert_eq!(response_line, encode_line(&expected_response));
            assert_eq!(
                handler.events,
                vec![EditorEvent::TargetRev { rev: 42 }, EditorEvent::CloseEdit]
            );
        });
    }

    #[test]
    fn drive_editor_normalizes_paths() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;

            struct Collector {
                events: Vec<EditorEvent>,
            }

            impl EditorEventHandler for Collector {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    self.events.push(event);
                    Ok(())
                }
            }

            let delete_entry = SvnItem::List(vec![
                SvnItem::Word("delete-entry".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"//trunk\\\\sub//./file.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(5)]),
                    SvnItem::String(b"d".to_vec()),
                ]),
            ]);

            let server_task = tokio::spawn(async move {
                write_item_line(&mut server, &delete_entry).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("close-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;
                read_line(&mut server).await
            });

            let mut handler = Collector { events: Vec::new() };
            let status = drive_editor(&mut conn, Some(&mut handler), false)
                .await
                .unwrap();
            assert!(matches!(status, EditorDriveStatus::Completed));

            let response_line = server_task.await.unwrap();
            let expected_response = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);
            assert_eq!(response_line, encode_line(&expected_response));
            assert_eq!(
                handler.events,
                vec![
                    EditorEvent::DeleteEntry {
                        path: "trunk/sub/file.txt".to_string(),
                        rev: 5,
                        dir_token: "d".to_string(),
                    },
                    EditorEvent::CloseEdit
                ]
            );
        });
    }

    #[test]
    fn drive_editor_rejects_unsafe_paths() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;

            struct Collector {
                events: Vec<EditorEvent>,
            }

            impl EditorEventHandler for Collector {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    self.events.push(event);
                    Ok(())
                }
            }

            let expected_failure = SvnItem::List(vec![
                SvnItem::Word("failure".to_string()),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::Number(1),
                    SvnItem::String(b"invalid path: unsafe path".to_vec()),
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                ])]),
            ]);

            let server_task = tokio::spawn(async move {
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("delete-entry".to_string()),
                        SvnItem::List(vec![
                            SvnItem::String(b"trunk/../x".to_vec()),
                            SvnItem::List(vec![SvnItem::Number(1)]),
                            SvnItem::String(b"d".to_vec()),
                        ]),
                    ]),
                )
                .await;

                let failure_line = read_line(&mut server).await;

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("abort-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;

                (failure_line, server)
            });

            let mut handler = Collector { events: Vec::new() };
            let status = drive_editor(&mut conn, Some(&mut handler), false)
                .await
                .unwrap();
            assert!(matches!(
                status,
                EditorDriveStatus::Aborted(SvnError::InvalidPath(_))
            ));

            let (failure_line, mut server) = server_task.await.unwrap();
            assert_eq!(failure_line, encode_line(&expected_failure));
            assert!(handler.events.is_empty());

            let no_response =
                tokio::time::timeout(Duration::from_millis(50), read_line(&mut server)).await;
            assert!(no_response.is_err());
        });
    }

    #[test]
    fn drive_editor_sends_failure_and_drains_on_handler_error() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;

            struct Failer;

            impl EditorEventHandler for Failer {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    if matches!(event, EditorEvent::TargetRev { .. }) {
                        return Err(SvnError::Protocol("boom".into()));
                    }
                    Ok(())
                }
            }

            let expected_failure = SvnItem::List(vec![
                SvnItem::Word("failure".to_string()),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::Number(1),
                    SvnItem::String(b"protocol error: boom".to_vec()),
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                ])]),
            ]);

            let server_task = tokio::spawn(async move {
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("target-rev".to_string()),
                        SvnItem::List(vec![SvnItem::Number(1)]),
                    ]),
                )
                .await;

                let failure_line = read_line(&mut server).await;

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("abort-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;

                (failure_line, server)
            });

            let mut handler = Failer;
            let status = drive_editor(&mut conn, Some(&mut handler), false)
                .await
                .unwrap();
            assert!(matches!(
                status,
                EditorDriveStatus::Aborted(SvnError::Protocol(msg)) if msg == "boom"
            ));

            let (failure_line, mut server) = server_task.await.unwrap();
            assert_eq!(failure_line, encode_line(&expected_failure));

            let no_response =
                tokio::time::timeout(Duration::from_millis(50), read_line(&mut server)).await;
            assert!(no_response.is_err());
        });
    }

    #[test]
    fn drive_editor_sends_failure_instead_of_success_on_close_edit_handler_error() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;

            struct Failer;

            impl EditorEventHandler for Failer {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    if matches!(event, EditorEvent::CloseEdit) {
                        return Err(SvnError::Protocol("boom".into()));
                    }
                    Ok(())
                }
            }

            let expected_failure = SvnItem::List(vec![
                SvnItem::Word("failure".to_string()),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::Number(1),
                    SvnItem::String(b"protocol error: boom".to_vec()),
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                ])]),
            ]);

            let server_task = tokio::spawn(async move {
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("close-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;
                read_line(&mut server).await
            });

            let mut handler = Failer;
            let status = drive_editor(&mut conn, Some(&mut handler), false)
                .await
                .unwrap();
            assert!(matches!(
                status,
                EditorDriveStatus::Aborted(SvnError::Protocol(msg)) if msg == "boom"
            ));

            let response_line = server_task.await.unwrap();
            assert_eq!(response_line, encode_line(&expected_failure));
        });
    }

    #[test]
    fn drive_editor_async_collects_events_and_sends_success() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;

            struct Collector {
                events: Vec<EditorEvent>,
            }

            impl AsyncEditorEventHandler for Collector {
                fn on_event<'a>(
                    &'a mut self,
                    event: EditorEvent,
                ) -> Pin<Box<dyn Future<Output = Result<(), SvnError>> + Send + 'a>>
                {
                    Box::pin(async move {
                        self.events.push(event);
                        Ok(())
                    })
                }
            }

            let server_task = tokio::spawn(async move {
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("target-rev".to_string()),
                        SvnItem::List(vec![SvnItem::Number(42)]),
                    ]),
                )
                .await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("close-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;
                read_line(&mut server).await
            });

            let mut handler = Collector { events: Vec::new() };
            let status = drive_editor_async(&mut conn, Some(&mut handler), false)
                .await
                .unwrap();
            assert!(matches!(status, EditorDriveStatus::Completed));

            let response_line = server_task.await.unwrap();
            let expected_response = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);
            assert_eq!(response_line, encode_line(&expected_response));
            assert_eq!(
                handler.events,
                vec![EditorEvent::TargetRev { rev: 42 }, EditorEvent::CloseEdit]
            );
        });
    }

    #[test]
    fn drive_editor_async_sends_failure_and_drains_on_handler_error() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;

            struct Failer;

            impl AsyncEditorEventHandler for Failer {
                fn on_event<'a>(
                    &'a mut self,
                    event: EditorEvent,
                ) -> Pin<Box<dyn Future<Output = Result<(), SvnError>> + Send + 'a>>
                {
                    Box::pin(async move {
                        if matches!(event, EditorEvent::TargetRev { .. }) {
                            return Err(SvnError::Protocol("boom".into()));
                        }
                        Ok(())
                    })
                }
            }

            let expected_failure = SvnItem::List(vec![
                SvnItem::Word("failure".to_string()),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::Number(1),
                    SvnItem::String(b"protocol error: boom".to_vec()),
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                ])]),
            ]);

            let server_task = tokio::spawn(async move {
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("target-rev".to_string()),
                        SvnItem::List(vec![SvnItem::Number(1)]),
                    ]),
                )
                .await;

                let failure_line = read_line(&mut server).await;

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("abort-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;

                (failure_line, server)
            });

            let mut handler = Failer;
            let status = drive_editor_async(&mut conn, Some(&mut handler), false)
                .await
                .unwrap();
            assert!(matches!(
                status,
                EditorDriveStatus::Aborted(SvnError::Protocol(msg)) if msg == "boom"
            ));

            let (failure_line, mut server) = server_task.await.unwrap();
            assert_eq!(failure_line, encode_line(&expected_failure));

            let no_response =
                tokio::time::timeout(Duration::from_millis(50), read_line(&mut server)).await;
            assert!(no_response.is_err());
        });
    }

    #[test]
    fn drive_editor_async_sends_failure_instead_of_success_on_close_edit_handler_error() {
        run_async(async {
            let (mut conn, mut server) = connected_conn().await;

            struct Failer;

            impl AsyncEditorEventHandler for Failer {
                fn on_event<'a>(
                    &'a mut self,
                    event: EditorEvent,
                ) -> Pin<Box<dyn Future<Output = Result<(), SvnError>> + Send + 'a>>
                {
                    Box::pin(async move {
                        if matches!(event, EditorEvent::CloseEdit) {
                            return Err(SvnError::Protocol("boom".into()));
                        }
                        Ok(())
                    })
                }
            }

            let expected_failure = SvnItem::List(vec![
                SvnItem::Word("failure".to_string()),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::Number(1),
                    SvnItem::String(b"protocol error: boom".to_vec()),
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                ])]),
            ]);

            let server_task = tokio::spawn(async move {
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("close-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;
                read_line(&mut server).await
            });

            let mut handler = Failer;
            let status = drive_editor_async(&mut conn, Some(&mut handler), false)
                .await
                .unwrap();
            assert!(matches!(
                status,
                EditorDriveStatus::Aborted(SvnError::Protocol(msg)) if msg == "boom"
            ));

            let response_line = server_task.await.unwrap();
            assert_eq!(response_line, encode_line(&expected_failure));
        });
    }
}
