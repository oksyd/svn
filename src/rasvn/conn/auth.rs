use super::*;

impl RaSvnConnection {
    pub(crate) async fn handle_auth_request_initial(&mut self) -> Result<(), SvnError> {
        let auth_req = self.read_command_response().await?;
        self.handle_auth_request_response(&auth_req).await
    }

    pub(crate) async fn handle_auth_request(&mut self) -> Result<(), SvnError> {
        let auth_req = self.read_command_response().await?;
        self.handle_auth_request_response(&auth_req).await
    }

    async fn handle_auth_request_response(
        &mut self,
        auth_req: &CommandResponse,
    ) -> Result<(), SvnError> {
        if auth_req.is_failure() {
            if let Some(first) = auth_req.errors.first().and_then(|e| e.as_list())
                && first.len() >= 4
            {
                let code = first[0].as_u64().unwrap_or_default();
                let message = first[1]
                    .as_string()
                    .unwrap_or_else(|| "<non-utf8>".to_string());
                let file = first[2]
                    .as_string()
                    .unwrap_or_else(|| "<non-utf8>".to_string());
                let line = first[3].as_u64().unwrap_or_default();
                debug!(code, message = %message, file = %file, line, "auth-request failed");
            }
            debug!(
                message = %auth_req.failure_message(),
                "auth-request command response is failure"
            );
        }
        let params = auth_req.success_params("auth-request")?;
        if params.len() < 2 {
            return Err(SvnError::Protocol("auth-request params too short".into()));
        }
        let mechs = params[0]
            .as_list()
            .ok_or_else(|| SvnError::Protocol("auth mechs not a list".into()))?;
        if mechs.is_empty() {
            debug!("auth-request has empty mechanism list (no auth required)");
            return Ok(());
        }
        let realm = params[1]
            .as_string()
            .unwrap_or_else(|| "<unknown>".to_string());
        debug!(realm = %realm, "server requires authentication");

        let mech_words: Vec<String> = mechs.into_iter().filter_map(|m| m.as_word()).collect();
        debug!(mechs = ?mech_words, "auth mechanisms offered");

        match self.handle_auth_request_builtin(&mech_words).await {
            Ok(()) => Ok(()),
            Err(builtin_err) => {
                #[cfg(feature = "cyrus-sasl")]
                {
                    if matches!(
                        &builtin_err,
                        SvnError::AuthUnavailable | SvnError::AuthFailed(_)
                    ) {
                        match self.handle_auth_request_cyrus_sasl(&mech_words).await {
                            Ok(()) => Ok(()),
                            Err(SvnError::AuthUnavailable) => Err(builtin_err),
                            Err(err) => Err(err),
                        }
                    } else {
                        Err(builtin_err)
                    }
                }

                #[cfg(not(feature = "cyrus-sasl"))]
                {
                    Err(builtin_err)
                }
            }
        }
    }

    async fn handle_auth_request_builtin(&mut self, mechs: &[String]) -> Result<(), SvnError> {
        let mechs_to_try = self.select_mechs(mechs)?;
        let mut last_failure = None::<String>;

        for (mech, initial) in mechs_to_try {
            debug!(mech = %mech, "trying auth mechanism");

            let token_tuple = match initial {
                Some(token) => SvnItem::List(vec![SvnItem::String(token)]),
                None => SvnItem::List(Vec::new()),
            };
            self.write_item(&SvnItem::List(vec![
                SvnItem::Word(mech.clone()),
                token_tuple,
            ]))
            .await?;

            loop {
                let challenge = self.read_item().await?;
                let SvnItem::List(parts) = challenge else {
                    return Err(SvnError::Protocol("invalid auth challenge".into()));
                };
                let Some(kind) = parts.first().and_then(|i| i.as_word()) else {
                    return Err(SvnError::Protocol("invalid auth challenge kind".into()));
                };
                match kind.as_str() {
                    "step" => {
                        debug!(mech = %mech, "auth challenge step");
                        let token = parts
                            .get(1)
                            .and_then(|i| i.as_list())
                            .and_then(|list| list.first().and_then(|i| i.as_bytes_string()))
                            .ok_or_else(|| SvnError::Protocol("auth step missing token".into()))?;
                        let reply = self.auth_step_reply(&mech, token)?;
                        self.write_item(&SvnItem::String(reply)).await?;
                    }
                    "success" => return Ok(()),
                    "failure" => {
                        let message = parts
                            .get(1)
                            .and_then(|i| i.as_list())
                            .and_then(|list| list.first().and_then(|i| i.as_string()))
                            .unwrap_or_else(|| "auth failed".to_string());
                        debug!(mech = %mech, message = %message, "auth mechanism failed");
                        last_failure = Some(message);
                        break;
                    }
                    other => {
                        return Err(SvnError::Protocol(format!(
                            "unexpected auth challenge: {other}"
                        )));
                    }
                }
            }
        }

        Err(SvnError::AuthFailed(
            last_failure.unwrap_or_else(|| "auth failed".to_string()),
        ))
    }

    #[cfg(feature = "cyrus-sasl")]
    async fn handle_auth_request_cyrus_sasl(&mut self, mechs: &[String]) -> Result<(), SvnError> {
        let mechstring = if mechs.iter().any(|m| m == "EXTERNAL") {
            "EXTERNAL".to_string()
        } else if mechs.iter().any(|m| m == "ANONYMOUS") {
            "ANONYMOUS".to_string()
        } else {
            mechs.join(" ")
        };

        let mechlist = std::ffi::CString::new(mechstring)
            .map_err(|_| SvnError::Protocol("SASL mech list contains NUL byte".into()))?;
        let mechlist = mechlist.as_c_str();

        let mut sasl = CyrusSasl::new(
            &self.host,
            self.username.as_deref(),
            self.password.as_deref(),
            false,
            self.local_addrport.as_deref(),
            self.remote_addrport.as_deref(),
        )?;

        let (mech, initial, mut rc) = sasl.client_start(mechlist)?;

        let mut initial_token = None;
        if initial.is_some() || mech == "EXTERNAL" {
            let raw = initial.unwrap_or_default();
            initial_token = Some(base64_encode(&raw));
        }

        let token_tuple = match initial_token {
            Some(token) => SvnItem::List(vec![SvnItem::String(token)]),
            None => SvnItem::List(Vec::new()),
        };
        self.write_item(&SvnItem::List(vec![
            SvnItem::Word(mech.clone()),
            token_tuple,
        ]))
        .await?;

        let mut last_status = None::<String>;
        while rc == SASL_CONTINUE {
            let challenge = self.read_item().await?;
            let SvnItem::List(parts) = challenge else {
                return Err(SvnError::Protocol("invalid SASL challenge".into()));
            };
            let Some(kind) = parts.first().and_then(|i| i.as_word()) else {
                return Err(SvnError::Protocol("invalid SASL challenge kind".into()));
            };
            last_status = Some(kind.clone());

            match kind.as_str() {
                "failure" => {
                    let message = parts
                        .get(1)
                        .and_then(|i| i.as_list())
                        .and_then(|list| list.first().and_then(|i| i.as_string()))
                        .unwrap_or_else(|| "auth failed".to_string());
                    return Err(SvnError::AuthFailed(message));
                }
                "success" | "step" => {}
                other => {
                    return Err(SvnError::Protocol(format!(
                        "unexpected SASL challenge: {other}"
                    )));
                }
            }

            let token = parts
                .get(1)
                .and_then(|i| i.as_list())
                .and_then(|list| list.first().and_then(|i| i.as_bytes_string()))
                .ok_or_else(|| SvnError::Protocol("SASL step missing token".into()))?;
            let token = if mech == "CRAM-MD5" {
                token
            } else {
                base64_decode(&token)?
            };

            let (out, next_rc) = sasl.client_step(&token)?;
            rc = next_rc;

            if kind == "success" {
                break;
            }

            let out = out.unwrap_or_default();
            let out = if mech == "CRAM-MD5" {
                out
            } else {
                base64_encode(&out)
            };
            self.write_item(&SvnItem::String(out)).await?;
        }

        if !matches!(last_status.as_deref(), Some("success")) {
            let item = self.read_item().await?;
            let SvnItem::List(parts) = item else {
                return Err(SvnError::Protocol("invalid SASL final response".into()));
            };
            let Some(kind) = parts.first().and_then(|i| i.as_word()) else {
                return Err(SvnError::Protocol("invalid SASL final kind".into()));
            };
            match kind.as_str() {
                "success" => {}
                "failure" => {
                    let message = parts
                        .get(1)
                        .and_then(|i| i.as_list())
                        .and_then(|list| list.first().and_then(|i| i.as_string()))
                        .unwrap_or_else(|| "auth failed".to_string());
                    return Err(SvnError::AuthFailed(message));
                }
                other => {
                    return Err(SvnError::Protocol(format!(
                        "unexpected SASL final response: {other}"
                    )));
                }
            }
        }

        let ssf = sasl.ssf()?;
        if ssf > 0 {
            if self.pos < self.buf.len() {
                let encrypted = self.buf[self.pos..].to_vec();
                self.buf.clear();
                self.pos = 0;

                let decoded = sasl.decode(&encrypted)?;
                self.buf.extend_from_slice(&decoded);
            } else if self.pos > 0 {
                self.buf.clear();
                self.pos = 0;
            }

            self.sasl = Some(Box::new(sasl));
        }
        Ok(())
    }

    fn select_mechs(&self, mechs: &[String]) -> Result<AuthMechanismChoices, SvnError> {
        let has_user = self.username.as_ref().is_some_and(|u| !u.trim().is_empty());
        let has_pass = self.password.as_ref().is_some();

        let mut out = Vec::new();

        if self.is_tunneled && mechs.iter().any(|m| m == "EXTERNAL") {
            out.push(("EXTERNAL".to_string(), Some(Vec::new())));
        }

        if has_user && has_pass && mechs.iter().any(|m| m == "CRAM-MD5") {
            out.push(("CRAM-MD5".to_string(), None));
        }

        if has_user && has_pass && mechs.iter().any(|m| m == "PLAIN") {
            let user = self.username.clone().unwrap_or_default();
            let pass = self.password.clone().unwrap_or_default();
            let mut token = Vec::with_capacity(user.len() + pass.len() + 2);
            token.push(0);
            token.extend_from_slice(user.as_bytes());
            token.push(0);
            token.extend_from_slice(pass.as_bytes());
            out.push(("PLAIN".to_string(), Some(token)));
        }

        if mechs.iter().any(|m| m == "ANONYMOUS") {
            out.push(("ANONYMOUS".to_string(), Some(Vec::new())));
        }

        if out.is_empty() && mechs.iter().any(|m| m == "EXTERNAL") {
            out.push(("EXTERNAL".to_string(), Some(Vec::new())));
        }

        if out.is_empty() {
            Err(SvnError::AuthUnavailable)
        } else {
            Ok(out)
        }
    }

    #[cfg(test)]
    pub(super) fn select_mech(&self, mechs: &[String]) -> Result<AuthMechanismChoice, SvnError> {
        let Some(choice) = self.select_mechs(mechs)?.into_iter().next() else {
            return Err(SvnError::AuthUnavailable);
        };
        Ok(choice)
    }

    pub(super) fn auth_step_reply(
        &self,
        mech: &str,
        challenge: Vec<u8>,
    ) -> Result<Vec<u8>, SvnError> {
        match mech {
            "CRAM-MD5" => {
                let user = self
                    .username
                    .clone()
                    .ok_or_else(|| SvnError::AuthFailed("missing username".into()))?;
                let pass = self
                    .password
                    .clone()
                    .ok_or_else(|| SvnError::AuthFailed("missing password".into()))?;
                let mut mac = Hmac::<Md5>::new_from_slice(pass.as_bytes())
                    .map_err(|_| SvnError::Protocol("failed to create HMAC-MD5".into()))?;
                mac.update(&challenge);
                let digest = mac.finalize().into_bytes();
                let hex = hex::encode(digest);
                Ok(format!("{user} {hex}").into_bytes())
            }
            "PLAIN" | "ANONYMOUS" | "EXTERNAL" => Err(SvnError::Protocol(format!(
                "unexpected auth step for {mech}"
            ))),
            other => Err(SvnError::Protocol(format!(
                "auth step not implemented for {other}"
            ))),
        }
    }
}
