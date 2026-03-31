use super::*;

impl RaSvnConnection {
    pub(crate) async fn write_cmd_success(&mut self) -> Result<(), SvnError> {
        self.write_item(&SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]))
        .await
    }

    pub(crate) async fn write_cmd_failure_early(
        &mut self,
        err: &SvnError,
    ) -> Result<bool, SvnError> {
        let message = err.to_string();
        let item = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(1),
                SvnItem::String(message.into_bytes()),
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
            ])]),
        ]);

        let mut buf = Vec::new();
        encode_item(&item, &mut buf);
        buf.push(b'\n');

        #[cfg(feature = "cyrus-sasl")]
        let wire = if let Some(sasl) = self.sasl.as_mut() {
            let max = sasl.max_outbuf() as usize;
            let mut out = Vec::new();
            let mut offset = 0usize;
            while offset < buf.len() {
                let take = if max == 0 {
                    buf.len() - offset
                } else {
                    (buf.len() - offset).min(max)
                };
                let chunk = &buf[offset..offset + take];
                out.extend_from_slice(&sasl.encode(chunk)?);
                offset += take;
            }
            out
        } else {
            buf
        };

        #[cfg(not(feature = "cyrus-sasl"))]
        let wire = buf;

        let deadline = Instant::now() + self.write_timeout;
        let mut offset = 0usize;
        let mut done = false;
        while offset < wire.len() {
            if Instant::now() >= deadline {
                return Err(SvnError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "write timed out",
                )));
            }

            match tokio::time::timeout(Duration::from_millis(0), self.write.write(&wire[offset..]))
                .await
            {
                Ok(Ok(0)) => {
                    return Err(SvnError::Io(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "write returned 0 bytes",
                    )));
                }
                Ok(Ok(n)) => offset += n,
                Ok(Err(err)) => return Err(SvnError::Io(err)),
                Err(_) => {
                    if !self.data_available().await? {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                        continue;
                    }

                    let item = self.read_item().await?;
                    if let SvnItem::List(parts) = item
                        && let Some(cmd) = parts.first().and_then(|i| i.as_word())
                        && cmd == "abort-edit"
                    {
                        done = true;
                    }
                }
            }
        }

        self.write.flush().await?;
        Ok(done)
    }

    pub(crate) async fn write_wire_bytes(&mut self, cleartext: &[u8]) -> Result<(), SvnError> {
        #[cfg(feature = "cyrus-sasl")]
        if let Some(sasl) = self.sasl.as_mut() {
            let max = sasl.max_outbuf() as usize;
            let mut offset = 0usize;
            while offset < cleartext.len() {
                let take = if max == 0 {
                    cleartext.len() - offset
                } else {
                    (cleartext.len() - offset).min(max)
                };
                let chunk = &cleartext[offset..offset + take];
                let encoded = sasl.encode(chunk)?;
                tokio::time::timeout(self.write_timeout, self.write.write_all(&encoded))
                    .await
                    .map_err(|_| {
                        SvnError::Io(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "write timed out",
                        ))
                    })??;
                offset += take;
            }
        } else {
            tokio::time::timeout(self.write_timeout, self.write.write_all(cleartext))
                .await
                .map_err(|_| {
                    SvnError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "write timed out",
                    ))
                })??;
        }

        #[cfg(not(feature = "cyrus-sasl"))]
        {
            tokio::time::timeout(self.write_timeout, self.write.write_all(cleartext))
                .await
                .map_err(|_| {
                    SvnError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "write timed out",
                    ))
                })??;
        }

        self.write.flush().await?;
        Ok(())
    }

    pub(super) async fn write_item(&mut self, item: &SvnItem) -> Result<(), SvnError> {
        self.write_buf.clear();
        encode_item(item, &mut self.write_buf);
        self.write_buf.push(b'\n');

        let buf = std::mem::take(&mut self.write_buf);
        let result = self.write_wire_bytes(&buf).await;
        self.write_buf = buf;
        result
    }

    pub(crate) async fn read_command_response(&mut self) -> Result<CommandResponse, SvnError> {
        let item = self.read_item().await?;
        let SvnItem::List(parts) = item else {
            return Err(SvnError::Protocol("command response not a list".into()));
        };
        if parts.is_empty() {
            return Err(SvnError::Protocol("empty command response".into()));
        }
        let kind = parts[0]
            .as_word()
            .ok_or_else(|| SvnError::Protocol("command response kind not a word".into()))?;
        match kind.as_str() {
            "success" => {
                let params = parts.get(1).and_then(|i| i.as_list()).unwrap_or_default();
                Ok(CommandResponse {
                    success: true,
                    params,
                    errors: Vec::new(),
                })
            }
            "failure" => {
                let errs = parts.get(1).and_then(|i| i.as_list()).unwrap_or_default();
                Ok(CommandResponse {
                    success: false,
                    params: Vec::new(),
                    errors: errs,
                })
            }
            other => Err(SvnError::Protocol(format!(
                "unexpected command response kind: {other}"
            ))),
        }
    }

    pub(crate) async fn read_item(&mut self) -> Result<SvnItem, SvnError> {
        tokio::time::timeout(self.read_timeout, self.read_item_inner())
            .await
            .map_err(|_| {
                SvnError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "read timed out",
                ))
            })?
    }

    pub(crate) async fn data_available(&mut self) -> Result<bool, SvnError> {
        while self.pos < self.buf.len() && self.buf[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        if self.pos < self.buf.len() {
            return Ok(true);
        }
        if self.pos > 0 {
            let len = self.buf.len();
            self.buf.copy_within(self.pos..len, 0);
            self.buf.truncate(len - self.pos);
            self.pos = 0;
        }

        let mut temp = [0u8; 16384];
        match tokio::time::timeout(Duration::from_millis(0), self.read.read(&mut temp)).await {
            Ok(Ok(n)) => {
                if n == 0 {
                    return Err(SvnError::Protocol("unexpected EOF".into()));
                }

                #[cfg(feature = "cyrus-sasl")]
                if let Some(sasl) = self.sasl.as_mut() {
                    let decoded = sasl.decode(&temp[..n])?;
                    self.buf.extend_from_slice(&decoded);
                } else {
                    self.buf.extend_from_slice(&temp[..n]);
                }

                #[cfg(not(feature = "cyrus-sasl"))]
                {
                    self.buf.extend_from_slice(&temp[..n]);
                }

                while self.pos < self.buf.len() && self.buf[self.pos].is_ascii_whitespace() {
                    self.pos += 1;
                }
                if self.pos < self.buf.len() {
                    Ok(true)
                } else {
                    if self.pos > 0 {
                        let len = self.buf.len();
                        self.buf.copy_within(self.pos..len, 0);
                        self.buf.truncate(len - self.pos);
                        self.pos = 0;
                    }
                    Ok(false)
                }
            }
            Ok(Err(err)) => Err(SvnError::Io(err)),
            Err(_) => Ok(false),
        }
    }

    async fn read_item_inner(&mut self) -> Result<SvnItem, SvnError> {
        skip_ws(self).await?;
        let ch = self.peek_byte().await?;
        if ch == b'(' {
            return self.read_list().await;
        }
        self.read_atom().await
    }

    async fn read_list(&mut self) -> Result<SvnItem, SvnError> {
        self.consume_byte().await?;
        require_ws(self).await?;

        let mut stack: Vec<Vec<SvnItem>> = vec![Vec::new()];
        loop {
            skip_ws(self).await?;
            let next = self.peek_byte().await?;
            match next {
                b')' => {
                    self.consume_byte().await?;
                    require_ws(self).await?;

                    let completed = stack
                        .pop()
                        .ok_or_else(|| SvnError::Protocol("list stack underflow".into()))?;
                    let item = SvnItem::List(completed);
                    if let Some(parent) = stack.last_mut() {
                        parent.push(item);
                    } else {
                        return Ok(item);
                    }
                }
                b'(' => {
                    self.consume_byte().await?;
                    require_ws(self).await?;
                    stack.push(Vec::new());
                }
                _ => {
                    let atom = self.read_atom().await?;
                    stack
                        .last_mut()
                        .ok_or_else(|| SvnError::Protocol("list stack underflow".into()))?
                        .push(atom);
                }
            }
        }
    }

    async fn read_atom(&mut self) -> Result<SvnItem, SvnError> {
        skip_ws(self).await?;
        let ch = self.peek_byte().await?;
        match ch {
            b'0'..=b'9' => {
                let n = parse_digits(self).await?;
                let next = self.peek_byte().await?;
                if next == b':' {
                    self.consume_byte().await?;
                    let bytes = self.read_exact_vec(n as usize).await?;
                    require_ws(self).await?;
                    Ok(SvnItem::String(bytes))
                } else {
                    require_ws(self).await?;
                    Ok(SvnItem::Number(n))
                }
            }
            _ => {
                let word = parse_word(self).await?;
                let item = match word.as_str() {
                    "true" => SvnItem::Bool(true),
                    "false" => SvnItem::Bool(false),
                    _ => SvnItem::Word(word),
                };
                require_ws(self).await?;
                Ok(item)
            }
        }
    }

    async fn read_exact_vec(&mut self, n: usize) -> Result<Vec<u8>, SvnError> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            if self.pos < self.buf.len() {
                let take = (n - out.len()).min(self.buf.len() - self.pos);
                out.extend_from_slice(&self.buf[self.pos..self.pos + take]);
                self.pos += take;
            } else {
                self.fill().await?;
            }
        }
        Ok(out)
    }

    async fn fill(&mut self) -> Result<(), SvnError> {
        if self.pos > 0 {
            let len = self.buf.len();
            self.buf.copy_within(self.pos..len, 0);
            self.buf.truncate(len - self.pos);
            self.pos = 0;
        }
        let mut temp = [0u8; 16384];

        #[cfg(feature = "cyrus-sasl")]
        {
            loop {
                let n = self.read.read(&mut temp).await?;
                if n == 0 {
                    return Err(SvnError::Protocol("unexpected EOF".into()));
                }

                if let Some(sasl) = self.sasl.as_mut() {
                    let decoded = sasl.decode(&temp[..n])?;
                    if decoded.is_empty() {
                        continue;
                    }
                    self.buf.extend_from_slice(&decoded);
                    break;
                }

                self.buf.extend_from_slice(&temp[..n]);
                break;
            }
            Ok(())
        }

        #[cfg(not(feature = "cyrus-sasl"))]
        {
            let n = self.read.read(&mut temp).await?;
            if n == 0 {
                return Err(SvnError::Protocol("unexpected EOF".into()));
            }
            self.buf.extend_from_slice(&temp[..n]);
            Ok(())
        }
    }

    async fn peek_byte(&mut self) -> Result<u8, SvnError> {
        loop {
            if self.pos < self.buf.len() {
                return Ok(self.buf[self.pos]);
            }
            self.fill().await?;
        }
    }

    async fn consume_byte(&mut self) -> Result<u8, SvnError> {
        let b = self.peek_byte().await?;
        self.pos += 1;
        Ok(b)
    }
}

async fn skip_ws(conn: &mut RaSvnConnection) -> Result<(), SvnError> {
    loop {
        let b = conn.peek_byte().await?;
        if b.is_ascii_whitespace() {
            let _ = conn.consume_byte().await?;
            continue;
        }
        break;
    }
    Ok(())
}

async fn require_ws(conn: &mut RaSvnConnection) -> Result<(), SvnError> {
    let b = conn.consume_byte().await?;
    if b.is_ascii_whitespace() {
        Ok(())
    } else {
        Err(SvnError::Protocol("expected whitespace".into()))
    }
}

async fn parse_digits(conn: &mut RaSvnConnection) -> Result<u64, SvnError> {
    let mut n = 0u64;
    loop {
        let b = conn.peek_byte().await?;
        if !b.is_ascii_digit() {
            break;
        }
        let _ = conn.consume_byte().await?;
        n = n
            .checked_mul(10)
            .and_then(|v| v.checked_add((b - b'0') as u64))
            .ok_or_else(|| SvnError::Protocol("number overflow".into()))?;
    }
    Ok(n)
}

async fn parse_word(conn: &mut RaSvnConnection) -> Result<String, SvnError> {
    let mut bytes = Vec::new();
    loop {
        let b = conn.peek_byte().await?;
        if b.is_ascii_whitespace() {
            break;
        }
        if b == b'(' || b == b')' || b == b':' {
            return Err(SvnError::Protocol("invalid word token".into()));
        }
        bytes.push(conn.consume_byte().await?);
    }
    String::from_utf8(bytes).map_err(|_| SvnError::Protocol("non-utf8 word".into()))
}
