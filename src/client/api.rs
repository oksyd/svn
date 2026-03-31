use super::*;

impl RaSvnClient {
    /// Convenience wrapper for [`RaSvnSession::get_latest_rev`].
    pub async fn get_latest_rev(&self) -> Result<u64, SvnError> {
        let mut session = self.open_session().await?;
        session.get_latest_rev().await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file`].
    pub async fn get_file<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        path: &str,
        rev: u64,
        want_props: bool,
        out: &mut W,
        max_bytes: u64,
    ) -> Result<u64, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_file(path, rev, want_props, out, max_bytes)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file_bytes`].
    pub async fn get_file_bytes(
        &self,
        path: &str,
        rev: u64,
        max_bytes: u64,
    ) -> Result<Vec<u8>, SvnError> {
        let mut session = self.open_session().await?;
        session.get_file_bytes(path, rev, max_bytes).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file_with_options`].
    pub async fn get_file_with_options<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        path: &str,
        options: &GetFileOptions,
        out: &mut W,
    ) -> Result<GetFileResult, SvnError> {
        let mut session = self.open_session().await?;
        session.get_file_with_options(path, options, out).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file_with_result`].
    pub async fn get_file_with_result<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        path: &str,
        rev: u64,
        want_props: bool,
        out: &mut W,
        max_bytes: u64,
    ) -> Result<GetFileResult, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_file_with_result(path, rev, want_props, out, max_bytes)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::log`].
    pub async fn log(&self, start_rev: u64, end_rev: u64) -> Result<Vec<LogEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.log(start_rev, end_rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::log_with_options`].
    pub async fn log_with_options(&self, options: &LogOptions) -> Result<Vec<LogEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.log_with_options(options).await
    }

    /// Convenience wrapper for [`RaSvnSession::log_each`].
    pub async fn log_each<F>(&self, options: &LogOptions, on_entry: F) -> Result<(), SvnError>
    where
        F: FnMut(LogEntry) -> Result<(), SvnError> + Send,
    {
        let mut session = self.open_session().await?;
        session.log_each(options, on_entry).await
    }

    /// Convenience wrapper for [`RaSvnSession::log_each_retrying`].
    pub async fn log_each_retrying<F>(
        &self,
        options: &LogOptions,
        on_entry: F,
    ) -> Result<(), SvnError>
    where
        F: FnMut(LogEntry) -> Result<(), SvnError> + Send,
    {
        let mut session = self.open_session().await?;
        session.log_each_retrying(options, on_entry).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_dated_rev`].
    pub async fn get_dated_rev(&self, date: &str) -> Result<u64, SvnError> {
        let mut session = self.open_session().await?;
        session.get_dated_rev(date).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_mergeinfo`].
    pub async fn get_mergeinfo(
        &self,
        paths: &[String],
        rev: Option<u64>,
        inherit: MergeInfoInheritance,
        include_descendants: bool,
    ) -> Result<MergeInfoCatalog, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_mergeinfo(paths, rev, inherit, include_descendants)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::get_deleted_rev`].
    pub async fn get_deleted_rev(
        &self,
        path: &str,
        peg_rev: u64,
        end_rev: u64,
    ) -> Result<Option<u64>, SvnError> {
        let mut session = self.open_session().await?;
        session.get_deleted_rev(path, peg_rev, end_rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_locations`].
    pub async fn get_locations(
        &self,
        path: &str,
        peg_rev: u64,
        location_revs: &[u64],
    ) -> Result<Vec<LocationEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.get_locations(path, peg_rev, location_revs).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_location_segments`].
    pub async fn get_location_segments(
        &self,
        path: &str,
        peg_rev: u64,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
    ) -> Result<Vec<LocationSegment>, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_location_segments(path, peg_rev, start_rev, end_rev)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file_revs`].
    pub async fn get_file_revs(
        &self,
        path: &str,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
        include_merged_revisions: bool,
    ) -> Result<Vec<crate::FileRev>, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_file_revs(path, start_rev, end_rev, include_merged_revisions)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file_revs_each`].
    pub async fn get_file_revs_each<F>(
        &self,
        path: &str,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
        include_merged_revisions: bool,
        on_rev: F,
    ) -> Result<(), SvnError>
    where
        F: FnMut(crate::FileRev) -> Result<(), SvnError> + Send,
    {
        let mut session = self.open_session().await?;
        session
            .get_file_revs_each(path, start_rev, end_rev, include_merged_revisions, on_rev)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file_revs_with_contents`].
    pub async fn get_file_revs_with_contents(
        &self,
        path: &str,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
        include_merged_revisions: bool,
        max_bytes: u64,
    ) -> Result<Vec<crate::FileRevContents>, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_file_revs_with_contents(
                path,
                start_rev,
                end_rev,
                include_merged_revisions,
                max_bytes,
            )
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::rev_proplist`].
    pub async fn rev_proplist(&self, rev: u64) -> Result<PropertyList, SvnError> {
        let mut session = self.open_session().await?;
        session.rev_proplist(rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::rev_prop`].
    pub async fn rev_prop(&self, rev: u64, name: &str) -> Result<Option<Vec<u8>>, SvnError> {
        let mut session = self.open_session().await?;
        session.rev_prop(rev, name).await
    }

    /// Convenience wrapper for [`RaSvnSession::change_rev_prop`].
    pub async fn change_rev_prop(
        &self,
        rev: u64,
        name: &str,
        value: Option<Vec<u8>>,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.change_rev_prop(rev, name, value).await
    }

    /// Convenience wrapper for [`RaSvnSession::change_rev_prop2`].
    pub async fn change_rev_prop2(
        &self,
        rev: u64,
        name: &str,
        value: Option<Vec<u8>>,
        dont_care: bool,
        previous_value: Option<Vec<u8>>,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session
            .change_rev_prop2(rev, name, value, dont_care, previous_value)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::proplist`].
    pub async fn proplist(
        &self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Option<PropertyList>, SvnError> {
        let mut session = self.open_session().await?;
        session.proplist(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::propget`].
    pub async fn propget(
        &self,
        path: &str,
        rev: Option<u64>,
        name: &str,
    ) -> Result<Option<Vec<u8>>, SvnError> {
        let mut session = self.open_session().await?;
        session.propget(path, rev, name).await
    }

    /// Convenience wrapper for [`RaSvnSession::inherited_props`].
    pub async fn inherited_props(
        &self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Vec<InheritedProps>, SvnError> {
        let mut session = self.open_session().await?;
        session.inherited_props(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_lock`].
    pub async fn get_lock(&self, path: &str) -> Result<Option<LockDesc>, SvnError> {
        let mut session = self.open_session().await?;
        session.get_lock(path).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_locks`].
    pub async fn get_locks(&self, path: &str, depth: Depth) -> Result<Vec<LockDesc>, SvnError> {
        let mut session = self.open_session().await?;
        session.get_locks(path, depth).await
    }

    /// Convenience wrapper for [`RaSvnSession::lock`].
    pub async fn lock(&self, path: &str, options: &LockOptions) -> Result<LockDesc, SvnError> {
        let mut session = self.open_session().await?;
        session.lock(path, options).await
    }

    /// Convenience wrapper for [`RaSvnSession::lock_many`].
    pub async fn lock_many(
        &self,
        options: &LockManyOptions,
        targets: &[LockTarget],
    ) -> Result<Vec<Result<LockDesc, SvnError>>, SvnError> {
        let mut session = self.open_session().await?;
        session.lock_many(options, targets).await
    }

    /// Convenience wrapper for [`RaSvnSession::unlock`].
    pub async fn unlock(&self, path: &str, options: &UnlockOptions) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.unlock(path, options).await
    }

    /// Convenience wrapper for [`RaSvnSession::unlock_many`].
    pub async fn unlock_many(
        &self,
        options: &UnlockManyOptions,
        targets: &[UnlockTarget],
    ) -> Result<Vec<Result<String, SvnError>>, SvnError> {
        let mut session = self.open_session().await?;
        session.unlock_many(options, targets).await
    }

    /// Convenience wrapper for [`RaSvnSession::commit`].
    pub async fn commit(
        &self,
        options: &CommitOptions,
        commands: &[EditorCommand],
    ) -> Result<CommitInfo, SvnError> {
        let mut session = self.open_session().await?;
        session.commit(options, commands).await
    }

    /// Convenience wrapper for [`RaSvnSession::list_dir`].
    pub async fn list_dir(&self, path: &str, rev: Option<u64>) -> Result<DirListing, SvnError> {
        let mut session = self.open_session().await?;
        session.list_dir(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::list_dir_with_fields`].
    pub async fn list_dir_with_fields(
        &self,
        path: &str,
        rev: Option<u64>,
        fields: &[DirentField],
    ) -> Result<DirListing, SvnError> {
        let mut session = self.open_session().await?;
        session.list_dir_with_fields(path, rev, fields).await
    }

    /// Convenience wrapper for [`RaSvnSession::check_path`].
    pub async fn check_path(&self, path: &str, rev: Option<u64>) -> Result<NodeKind, SvnError> {
        let mut session = self.open_session().await?;
        session.check_path(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::stat`].
    pub async fn stat(&self, path: &str, rev: Option<u64>) -> Result<Option<StatEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.stat(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::list`].
    pub async fn list(
        &self,
        path: &str,
        rev: Option<u64>,
        depth: Depth,
        fields: &[DirentField],
        patterns: Option<&[String]>,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.list(path, rev, depth, fields, patterns).await
    }

    /// Convenience wrapper for [`RaSvnSession::list_with_options`].
    pub async fn list_with_options(
        &self,
        options: &ListOptions,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.list_with_options(options).await
    }

    /// Convenience wrapper for [`RaSvnSession::list_with_options_each`].
    pub async fn list_with_options_each<F>(
        &self,
        options: &ListOptions,
        on_entry: F,
    ) -> Result<(), SvnError>
    where
        F: FnMut(DirEntry) -> Result<(), SvnError> + Send,
    {
        let mut session = self.open_session().await?;
        session.list_with_options_each(options, on_entry).await
    }

    /// Convenience wrapper for [`RaSvnSession::list_recursive`].
    pub async fn list_recursive(
        &self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.list_recursive(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::update`].
    pub async fn update(
        &self,
        options: &UpdateOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.update(options, report, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::update_with_async_handler`].
    pub async fn update_with_async_handler(
        &self,
        options: &UpdateOptions,
        report: &Report,
        handler: &mut dyn AsyncEditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session
            .update_with_async_handler(options, report, handler)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::switch`].
    pub async fn switch(
        &self,
        options: &SwitchOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.switch(options, report, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::switch_with_async_handler`].
    pub async fn switch_with_async_handler(
        &self,
        options: &SwitchOptions,
        report: &Report,
        handler: &mut dyn AsyncEditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session
            .switch_with_async_handler(options, report, handler)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::status`].
    pub async fn status(
        &self,
        options: &StatusOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.status(options, report, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::status_with_async_handler`].
    pub async fn status_with_async_handler(
        &self,
        options: &StatusOptions,
        report: &Report,
        handler: &mut dyn AsyncEditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session
            .status_with_async_handler(options, report, handler)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::diff`].
    pub async fn diff(
        &self,
        options: &DiffOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.diff(options, report, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::diff_with_async_handler`].
    pub async fn diff_with_async_handler(
        &self,
        options: &DiffOptions,
        report: &Report,
        handler: &mut dyn AsyncEditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session
            .diff_with_async_handler(options, report, handler)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::replay`].
    pub async fn replay(
        &self,
        options: &ReplayOptions,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.replay(options, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::replay_with_async_handler`].
    pub async fn replay_with_async_handler(
        &self,
        options: &ReplayOptions,
        handler: &mut dyn AsyncEditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.replay_with_async_handler(options, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::replay_range`].
    pub async fn replay_range(
        &self,
        options: &ReplayRangeOptions,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.replay_range(options, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::replay_range_with_async_handler`].
    pub async fn replay_range_with_async_handler(
        &self,
        options: &ReplayRangeOptions,
        handler: &mut dyn AsyncEditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session
            .replay_range_with_async_handler(options, handler)
            .await
    }
}
