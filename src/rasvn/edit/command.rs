use crate::path::validate_rel_path_ref;
use crate::{EditorCommand, SvnError};

use super::super::wire::WireEncoder;

#[cfg(test)]
pub(crate) async fn send_editor_command(
    conn: &mut super::super::conn::RaSvnConnection,
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
