use super::*;

impl RaSvnSession {
    /// Returns the node properties for a file or directory path.
    pub async fn proplist(
        &mut self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Option<PropertyList>, SvnError> {
        let path = validate_rel_dir_path(path)?;
        let kind = self.check_path(&path, rev).await?;
        match kind {
            NodeKind::None => Ok(None),
            NodeKind::Unknown => Err(SvnError::Protocol("node kind unknown".into())),
            NodeKind::File => {
                let path = validate_rel_path(&path)?;
                let props = self
                    .with_retry("get-file-proplist", move |conn| {
                        let path = path.clone();
                        Box::pin(async move {
                            let rev_tuple = match rev {
                                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                                None => SvnItem::List(Vec::new()),
                            };

                            let params = SvnItem::List(vec![
                                SvnItem::String(path.as_bytes().to_vec()),
                                rev_tuple,
                                SvnItem::Bool(true),  // want-props
                                SvnItem::Bool(false), // want-contents
                                // The standard client always sends want-iprops as false and
                                // uses a separate `get-iprops` request (see protocol notes).
                                SvnItem::Bool(false),
                            ]);

                            let response = conn.call("get-file", params).await?;
                            let params = response.success_params("get-file")?;
                            let meta = parse_get_file_response_params(params)?;
                            Ok(meta.props)
                        })
                    })
                    .await?;
                Ok(Some(props))
            }
            NodeKind::Dir => {
                let props = self
                    .with_retry("get-dir-proplist", move |conn| {
                        let path = path.clone();
                        Box::pin(async move {
                            let rev_tuple = match rev {
                                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                                None => SvnItem::List(Vec::new()),
                            };

                            let params = SvnItem::List(vec![
                                SvnItem::String(path.as_bytes().to_vec()),
                                rev_tuple,
                                SvnItem::Bool(true),  // want-props
                                SvnItem::Bool(false), // want-contents
                                SvnItem::List(Vec::new()),
                                // The standard client always sends want-iprops as false and
                                // uses a separate `get-iprops` request (see protocol notes).
                                SvnItem::Bool(false),
                            ]);

                            let response = conn.call("get-dir", params).await?;
                            let params = response.success_params("get-dir")?;
                            if params.len() < 2 {
                                return Err(SvnError::Protocol(
                                    "get-dir response missing props".into(),
                                ));
                            }
                            parse_proplist(&params[1])
                        })
                    })
                    .await?;
                Ok(Some(props))
            }
        }
    }

    /// Returns a single property value for a file or directory path.
    pub async fn propget(
        &mut self,
        path: &str,
        rev: Option<u64>,
        name: &str,
    ) -> Result<Option<Vec<u8>>, SvnError> {
        let Some(props) = self.proplist(path, rev).await? else {
            return Ok(None);
        };
        Ok(props.get(name).cloned())
    }

    /// Runs `get-iprops` and returns inherited properties for a path.
    pub async fn inherited_props(
        &mut self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Vec<InheritedProps>, SvnError> {
        let path = validate_rel_dir_path(path)?;
        self.with_retry("get-iprops", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };

                let params =
                    SvnItem::List(vec![SvnItem::String(path.as_bytes().to_vec()), rev_tuple]);
                let response = conn.call("get-iprops", params).await?;
                let params = response.success_params("get-iprops")?;
                let iproplist = params.first().ok_or_else(|| {
                    SvnError::Protocol("get-iprops response missing iproplist".into())
                })?;
                parse_iproplist(iproplist)
            })
        })
        .await
    }
}
