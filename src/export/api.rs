use super::*;

impl RaSvnSession {
    /// Exports a repository subtree to `dest` using `update`.
    ///
    /// This convenience helper builds a minimal "empty" report (`start_empty = true`)
    /// and drives [`crate::RaSvnSession::update_with_async_handler`] with a [`TokioFsEditor`].
    pub async fn export_to_dir(
        &mut self,
        options: &UpdateOptions,
        dest: impl AsRef<Path>,
    ) -> Result<(), SvnError> {
        let mut report = Report::new();
        report.push(ReportCommand::SetPath {
            path: String::new(),
            rev: 0,
            start_empty: true,
            lock_token: None,
            depth: options.depth,
        });
        report.finish();

        self.export_to_dir_with_report(options, &report, dest).await
    }

    /// Exports a repository subtree to `dest` using a caller-provided report.
    pub async fn export_to_dir_with_report(
        &mut self,
        options: &UpdateOptions,
        report: &Report,
        dest: impl AsRef<Path>,
    ) -> Result<(), SvnError> {
        let mut editor = TokioFsEditor::new(dest.as_ref().to_path_buf())
            .with_strip_prefix(options.target.clone());
        self.update_with_async_handler(options, report, &mut editor)
            .await?;
        Ok(())
    }
}

impl RaSvnClient {
    /// Convenience wrapper for [`RaSvnSession::export_to_dir`].
    pub async fn export_to_dir(
        &self,
        options: &UpdateOptions,
        dest: impl AsRef<Path>,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.export_to_dir(options, dest).await
    }
}
