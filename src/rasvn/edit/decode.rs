use crate::path::validate_rel_path;
use crate::rasvn::parse::{parse_proplist, parse_server_error};
use crate::raw::SvnItem;
use crate::{EditorEvent, SvnError};

pub(crate) fn parse_failure(params: &[SvnItem]) -> SvnError {
    SvnError::Server(parse_server_error(params))
}

pub(super) fn parse_editor_event(cmd: &str, params: &[SvnItem]) -> Result<EditorEvent, SvnError> {
    match cmd {
        "target-rev" => {
            let rev = params
                .first()
                .and_then(|item| item.as_u64())
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
                .and_then(|item| item.as_string())
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
                .and_then(|item| item.as_string())
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
        SvnItem::List(items) => items.first().and_then(|item| item.as_u64()),
        _ => item.as_u64(),
    }
}

fn opt_tuple_string(item: &SvnItem) -> Option<String> {
    match item {
        SvnItem::List(items) => items.first().and_then(|item| item.as_string()),
        _ => item.as_string(),
    }
}

fn opt_tuple_bytes(item: &SvnItem) -> Option<Vec<u8>> {
    match item {
        SvnItem::List(items) => items.first().and_then(|item| item.as_bytes_string()),
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
