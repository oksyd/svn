use crate::{CommitInfo, CommitOptions, RaSvnClient, RaSvnSession, SvnError};

use super::{CommitBuilder, CommitStreamBuilder};

impl RaSvnSession {
    /// Submits the changes in `builder` as one repository commit.
    ///
    /// Put/add/replace/delete paths are relative to the current session URL.
    /// Returns [`CommitInfo`] with the new revision after server confirmation.
    ///
    /// # Errors
    ///
    /// Local validation, authorization, hooks, or conflicting repository changes
    /// can reject the edit. A submitted commit is not automatically retried.
    /// A transport failure while awaiting confirmation leaves the outcome
    /// uncertain; inspect repository history before resubmitting.
    ///
    /// # Example
    ///
    /// This creates a repository revision when run against a writable session.
    ///
    /// ```rust,no_run
    /// use svn::{CommitBuilder, CommitInfo, CommitOptions, RaSvnSession};
    ///
    /// async fn write_greeting(session: &mut RaSvnSession) -> svn::Result<CommitInfo> {
    ///     let base_rev = session.get_latest_rev().await?;
    ///     let changes = CommitBuilder::new()
    ///         .with_base_rev(base_rev)
    ///         .put_file("hello.txt", b"hello from Rust\n".to_vec());
    ///     session
    ///         .commit_with_builder(&CommitOptions::new("Update greeting"), &changes)
    ///         .await
    /// }
    /// ```
    pub async fn commit_with_builder(
        &mut self,
        options: &CommitOptions,
        builder: &CommitBuilder,
    ) -> Result<CommitInfo, SvnError> {
        let commands = builder.build_editor_commands(self).await?;
        self.commit(options, &commands).await
    }

    /// Runs `commit`, streaming file contents from a [`CommitStreamBuilder`].
    ///
    /// Consumes the builder and its readers. See [`CommitStreamBuilder`] for an
    /// upload example. The commit outcome and failure semantics are the same
    /// as [`RaSvnSession::commit_with_builder`].
    pub async fn commit_with_stream_builder(
        &mut self,
        options: &CommitOptions,
        builder: CommitStreamBuilder,
    ) -> Result<CommitInfo, SvnError> {
        builder.commit(self, options).await
    }
}

impl RaSvnClient {
    /// Opens a session and runs `commit` from a high-level [`CommitBuilder`].
    pub async fn commit_with_builder(
        &self,
        options: &CommitOptions,
        builder: &CommitBuilder,
    ) -> Result<CommitInfo, SvnError> {
        let mut session = self.open_session().await?;
        session.commit_with_builder(options, builder).await
    }

    /// Opens a session and runs `commit` from a streaming [`CommitStreamBuilder`].
    pub async fn commit_with_stream_builder(
        &self,
        options: &CommitOptions,
        builder: CommitStreamBuilder,
    ) -> Result<CommitInfo, SvnError> {
        let mut session = self.open_session().await?;
        session.commit_with_stream_builder(options, builder).await
    }
}
