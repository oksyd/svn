use super::*;

impl RaSvnSession {
    /// Runs `get-latest-rev` and returns the latest (HEAD) revision number.
    pub async fn get_latest_rev(&mut self) -> Result<u64, SvnError> {
        self.with_retry("get-latest-rev", |conn| {
            Box::pin(async move {
                let response = conn
                    .call("get-latest-rev", SvnItem::List(Vec::new()))
                    .await?;
                let params = response.success_params("get-latest-rev")?;
                let rev = params
                    .first()
                    .and_then(|i| i.as_u64())
                    .ok_or_else(|| SvnError::Protocol("missing latest rev".into()))?;
                Ok(rev)
            })
        })
        .await
    }

    /// Runs `get-dated-rev` and returns the revision number for a given date.
    ///
    /// The date string is interpreted by the server.
    pub async fn get_dated_rev(&mut self, date: &str) -> Result<u64, SvnError> {
        let date = date.as_bytes().to_vec();
        self.with_retry("get-dated-rev", move |conn| {
            let date = date.clone();
            Box::pin(async move {
                let params = SvnItem::List(vec![SvnItem::String(date)]);
                let response = conn.call("get-dated-rev", params).await?;
                let params = response.success_params("get-dated-rev")?;
                let rev = params
                    .first()
                    .and_then(|i| i.as_u64())
                    .ok_or_else(|| SvnError::Protocol("missing dated rev".into()))?;
                Ok(rev)
            })
        })
        .await
    }

    /// Runs `get-mergeinfo` for a set of paths.
    pub async fn get_mergeinfo(
        &mut self,
        paths: &[String],
        rev: Option<u64>,
        inherit: MergeInfoInheritance,
        include_descendants: bool,
    ) -> Result<MergeInfoCatalog, SvnError> {
        let paths: Result<Vec<String>, SvnError> =
            paths.iter().map(|p| validate_rel_dir_path(p)).collect();
        let paths = paths?;

        self.with_retry("get-mergeinfo", move |conn| {
            let paths = paths.clone();
            Box::pin(async move {
                let target_paths = SvnItem::List(
                    paths
                        .iter()
                        .map(|p| SvnItem::String(p.as_bytes().to_vec()))
                        .collect(),
                );
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };
                let params = SvnItem::List(vec![
                    target_paths,
                    rev_tuple,
                    SvnItem::Word(inherit.as_word().to_string()),
                    SvnItem::Bool(include_descendants),
                ]);

                let response = conn.call("get-mergeinfo", params).await?;
                let params = response.success_params("get-mergeinfo")?;
                parse_mergeinfo_catalog(params)
            })
        })
        .await
    }
}
