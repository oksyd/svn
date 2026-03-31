use super::*;

impl RaSvnConnection {
    pub(crate) async fn call(
        &mut self,
        command: &str,
        params: SvnItem,
    ) -> Result<CommandResponse, SvnError> {
        self.send_command(command, params).await?;
        self.handle_auth_request().await?;
        self.read_command_response().await
    }

    pub(crate) async fn send_command(
        &mut self,
        command: &str,
        params: SvnItem,
    ) -> Result<(), SvnError> {
        self.write_buf.clear();
        encode_command_item(command, &params, &mut self.write_buf);
        self.write_buf.push(b'\n');

        let buf = std::mem::take(&mut self.write_buf);
        let result = self.write_wire_bytes(&buf).await;
        self.write_buf = buf;
        result
    }
}
