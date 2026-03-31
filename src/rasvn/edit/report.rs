use crate::path::{validate_rel_dir_path_ref, validate_rel_path_ref};
use crate::{Report, ReportCommand, SvnError};

use super::super::conn::RaSvnConnection;
use super::super::wire::WireEncoder;

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
