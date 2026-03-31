use crate::{CommitInfo, CommitOptions, RaSvnSession, SvnError};

use super::{CommitBuilder, CommitStreamBuilder};

impl RaSvnSession {
    /// Runs `commit`, building a low-level editor drive from `builder`.
    ///
    /// ```rust,no_run
    /// # use svn::{CommitBuilder, CommitOptions, RaSvnSession};
    /// # async fn demo(session: &mut RaSvnSession, head: u64) -> svn::Result<()> {
    /// let builder = CommitBuilder::new()
    ///     .with_base_rev(head)
    ///     .put_file("trunk/hello.txt", b"hello from svn-rs\n".to_vec());
    /// session
    ///     .commit_with_builder(&CommitOptions::new("edit file contents"), &builder)
    ///     .await?;
    /// # Ok(())
    /// # }
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
    pub async fn commit_with_stream_builder(
        &mut self,
        options: &CommitOptions,
        builder: CommitStreamBuilder,
    ) -> Result<CommitInfo, SvnError> {
        builder.commit(self, options).await
    }
}
