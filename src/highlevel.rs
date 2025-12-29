use similar::{ChangeTag, TextDiff};

use crate::{BlameLine, FileRevContents, PropertyList, RaSvnClient, RaSvnSession, SvnError};

fn prop_string(props: &PropertyList, name: &str) -> Option<String> {
    props
        .get(name)
        .and_then(|v| (!v.is_empty()).then(|| String::from_utf8_lossy(v).into_owned()))
}

impl RaSvnSession {
    /// Computes a unified diff between `old_rev` and `new_rev` for `path`.
    ///
    /// The file contents are decoded as UTF-8 lossily (invalid sequences are
    /// replaced).
    pub async fn diff_file_unified(
        &mut self,
        path: &str,
        old_rev: u64,
        new_rev: u64,
        max_bytes: u64,
    ) -> Result<String, SvnError> {
        let old = self.get_file_bytes(path, old_rev, max_bytes).await?;
        let new = self.get_file_bytes(path, new_rev, max_bytes).await?;

        let old_text = String::from_utf8_lossy(&old);
        let new_text = String::from_utf8_lossy(&new);

        let diff = TextDiff::from_lines(&old_text, &new_text);
        Ok(diff
            .unified_diff()
            .header(&format!("{path}@{old_rev}"), &format!("{path}@{new_rev}"))
            .to_string())
    }

    /// Returns a best-effort, line-based blame for `path` across a revision range.
    ///
    /// This is built from `get-file-revs` materialized contents and uses a
    /// line-based diff to attribute lines to the latest revision that changed
    /// them.
    pub async fn blame_file(
        &mut self,
        path: &str,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
        include_merged_revisions: bool,
        max_bytes: u64,
    ) -> Result<Vec<BlameLine>, SvnError> {
        let revs: Vec<FileRevContents> = self
            .get_file_revs_with_contents(
                path,
                start_rev,
                end_rev,
                include_merged_revisions,
                max_bytes,
            )
            .await?;

        let Some((first, rest)) = revs.split_first() else {
            return Ok(Vec::new());
        };

        let first_rev = first.file_rev.rev;
        let first_author = prop_string(&first.file_rev.rev_props, "svn:author");
        let first_date = prop_string(&first.file_rev.rev_props, "svn:date");

        let mut prev_text = String::from_utf8_lossy(&first.contents).into_owned();
        let mut blame = Vec::new();
        for change in TextDiff::from_lines("", &prev_text).iter_all_changes() {
            if matches!(change.tag(), ChangeTag::Insert | ChangeTag::Equal) {
                blame.push(BlameLine {
                    rev: first_rev,
                    author: first_author.clone(),
                    date: first_date.clone(),
                    line: change.value().to_string(),
                });
            }
        }

        for rev in rest {
            let cur_rev = rev.file_rev.rev;
            let cur_author = prop_string(&rev.file_rev.rev_props, "svn:author");
            let cur_date = prop_string(&rev.file_rev.rev_props, "svn:date");
            let cur_text = String::from_utf8_lossy(&rev.contents).into_owned();

            let diff = TextDiff::from_lines(&prev_text, &cur_text);
            let mut next = Vec::new();
            let mut old_idx = 0usize;
            for change in diff.iter_all_changes() {
                match change.tag() {
                    ChangeTag::Equal => {
                        let prev = blame
                            .get(old_idx)
                            .ok_or_else(|| SvnError::Protocol("blame diff out of bounds".into()))?;
                        next.push(prev.clone());
                        old_idx += 1;
                    }
                    ChangeTag::Delete => {
                        old_idx += 1;
                    }
                    ChangeTag::Insert => {
                        next.push(BlameLine {
                            rev: cur_rev,
                            author: cur_author.clone(),
                            date: cur_date.clone(),
                            line: change.value().to_string(),
                        });
                    }
                }
            }

            blame = next;
            prev_text = cur_text;
        }

        Ok(blame)
    }
}

impl RaSvnClient {
    /// Convenience wrapper for [`RaSvnSession::diff_file_unified`].
    pub async fn diff_file_unified(
        &self,
        path: &str,
        old_rev: u64,
        new_rev: u64,
        max_bytes: u64,
    ) -> Result<String, SvnError> {
        let mut session = self.open_session().await?;
        session
            .diff_file_unified(path, old_rev, new_rev, max_bytes)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::blame_file`].
    pub async fn blame_file(
        &self,
        path: &str,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
        include_merged_revisions: bool,
        max_bytes: u64,
    ) -> Result<Vec<BlameLine>, SvnError> {
        let mut session = self.open_session().await?;
        session
            .blame_file(
                path,
                start_rev,
                end_rev,
                include_merged_revisions,
                max_bytes,
            )
            .await
    }
}
