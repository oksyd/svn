use super::*;

impl RaSvnSession {
    /// Returns the location of `path@peg_rev` at each requested revision.
    pub async fn get_locations(
        &mut self,
        path: &str,
        peg_rev: u64,
        location_revs: &[u64],
    ) -> Result<Vec<LocationEntry>, SvnError> {
        let path = validate_rel_path(path)?;
        let revs = location_revs.to_vec();
        self.with_retry("get-locations", move |conn| {
            let path = path.clone();
            let revs = revs.clone();
            Box::pin(async move {
                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    SvnItem::Number(peg_rev),
                    SvnItem::List(revs.into_iter().map(SvnItem::Number).collect()),
                ]);

                conn.send_command("get-locations", params).await?;
                conn.handle_auth_request().await?;

                let mut out = Vec::new();
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(_) => out.push(parse_location_entry(item)?),
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected location entry item: {}",
                                other.kind()
                            )));
                        }
                    }
                }

                let response = conn.read_command_response().await?;
                if response.is_failure() {
                    return Err(response.failure("get-locations"));
                }
                Ok(out)
            })
        })
        .await
    }

    /// Runs `get-location-segments` and returns location segments for a path.
    pub async fn get_location_segments(
        &mut self,
        path: &str,
        peg_rev: u64,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
    ) -> Result<Vec<LocationSegment>, SvnError> {
        let path = validate_rel_path(path)?;
        self.with_retry("get-location-segments", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let peg_tuple = SvnItem::List(vec![SvnItem::Number(peg_rev)]);
                let start_tuple = match start_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };
                let end_tuple = match end_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };

                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    peg_tuple,
                    start_tuple,
                    end_tuple,
                ]);

                conn.send_command("get-location-segments", params).await?;
                conn.handle_auth_request().await?;

                let mut out = Vec::new();
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(_) => out.push(parse_location_segment(item)?),
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected location segment item: {}",
                                other.kind()
                            )));
                        }
                    }
                }

                let response = conn.read_command_response().await?;
                response.ensure_success("get-location-segments")?;
                Ok(out)
            })
        })
        .await
    }
}
