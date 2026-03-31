use super::*;

mod diff;
mod replay;
mod status;
mod switch;
mod update;

fn require_finish_report(report: &Report) -> Result<(), SvnError> {
    match report.commands.last() {
        Some(ReportCommand::FinishReport) => Ok(()),
        _ => Err(SvnError::Protocol(
            "report must end with finish-report".into(),
        )),
    }
}
