use super::*;

impl RaSvnSession {
    /// Runs `rev-proplist` and returns all revision properties for `rev`.
    pub async fn rev_proplist(&mut self, rev: u64) -> Result<PropertyList, SvnError> {
        self.with_retry("rev-proplist", move |conn| {
            Box::pin(async move {
                let response = conn
                    .call("rev-proplist", SvnItem::List(vec![SvnItem::Number(rev)]))
                    .await?;
                let params = response.success_params("rev-proplist")?;
                let proplist = params.first().ok_or_else(|| {
                    SvnError::Protocol("rev-proplist response missing proplist".into())
                })?;
                parse_proplist(proplist)
            })
        })
        .await
    }

    /// Runs `rev-prop` and returns a single revision property value.
    pub async fn rev_prop(&mut self, rev: u64, name: &str) -> Result<Option<Vec<u8>>, SvnError> {
        let name = name.as_bytes().to_vec();
        self.with_retry("rev-prop", move |conn| {
            let name = name.clone();
            Box::pin(async move {
                let params = SvnItem::List(vec![SvnItem::Number(rev), SvnItem::String(name)]);
                let response = conn.call("rev-prop", params).await?;
                let params = response.success_params("rev-prop")?;
                parse_rev_prop_value(params)
            })
        })
        .await
    }

    /// Runs `change-rev-prop` to set or delete a revision property.
    pub async fn change_rev_prop(
        &mut self,
        rev: u64,
        name: &str,
        value: Option<Vec<u8>>,
    ) -> Result<(), SvnError> {
        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let name = name.as_bytes().to_vec();
            let mut items = vec![SvnItem::Number(rev), SvnItem::String(name)];
            if let Some(value) = value {
                items.push(SvnItem::String(value));
            }

            let response = conn.call("change-rev-prop", SvnItem::List(items)).await?;
            let _ = response.success_params("change-rev-prop")?;
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `change-rev-prop2` to atomically set or delete a revision property.
    ///
    /// This requires the server to support `atomic-revprops`.
    pub async fn change_rev_prop2(
        &mut self,
        rev: u64,
        name: &str,
        value: Option<Vec<u8>>,
        dont_care: bool,
        previous_value: Option<Vec<u8>>,
    ) -> Result<(), SvnError> {
        if dont_care && previous_value.is_some() {
            return Err(SvnError::Protocol(
                "change-rev-prop2 previous_value must be None when dont_care is true".into(),
            ));
        }

        self.ensure_connected().await?;
        if self.server_info.is_some() {
            let conn = self.conn.as_ref().ok_or_else(|| {
                SvnError::Protocol("change-rev-prop2 requires a connected session".into())
            })?;
            if !conn.server_has_cap("atomic-revprops") {
                return Err(SvnError::Protocol(
                    "server does not support atomic revision property changes".into(),
                ));
            }
        }
        let result = async {
            let conn = self.conn_mut()?;

            let name = name.as_bytes().to_vec();
            let value_tuple = match value {
                Some(value) => SvnItem::List(vec![SvnItem::String(value)]),
                None => SvnItem::List(Vec::new()),
            };

            let mut cond_items = vec![SvnItem::Bool(dont_care)];
            if let Some(previous_value) = previous_value {
                cond_items.push(SvnItem::String(previous_value));
            }
            let cond_tuple = SvnItem::List(cond_items);

            let params = SvnItem::List(vec![
                SvnItem::Number(rev),
                SvnItem::String(name),
                value_tuple,
                cond_tuple,
            ]);

            let response = conn.call("change-rev-prop2", params).await?;
            let _ = response.success_params("change-rev-prop2")?;
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }
}

fn parse_rev_prop_value(params: &[SvnItem]) -> Result<Option<Vec<u8>>, SvnError> {
    if params.len() != 1 {
        return Err(SvnError::Protocol(
            "rev-prop response must contain exactly one value tuple".into(),
        ));
    }

    let items = params[0]
        .as_list()
        .ok_or_else(|| SvnError::Protocol("rev-prop value tuple not a list".into()))?;
    match items.as_slice() {
        [] => Ok(None),
        [value] => value
            .as_bytes_string()
            .map(Some)
            .ok_or_else(|| SvnError::Protocol("rev-prop value not a string".into())),
        _ => Err(SvnError::Protocol(
            "rev-prop value tuple must contain at most one value".into(),
        )),
    }
}
