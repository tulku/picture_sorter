use super::protocol::*;
use crate::{
    config::Config,
    error::{AppError, AppResult, ErrorCode},
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::MetadataExt, net::UnixStream},
    time::Duration,
};
pub struct Client {
    stream: UnixStream,
}
impl Client {
    pub fn connect(config: &Config) -> AppResult<Self> {
        crate::fs::RootHandle::open(&config.state_root, crate::fs::RootPolicy::Local)?;
        let socket = config.state_root.join("service.sock");
        let metadata = std::fs::symlink_metadata(&socket)?;
        // Private socket ownership and kernel peer credentials must agree before sending a request.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o600 {
            return Err(AppError::new(
                ErrorCode::Permission,
                "service socket must be owned by current user with mode 0600",
            ));
        }
        let stream = UnixStream::connect(socket)?;
        super::server::check_peer(&stream)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self { stream })
    }
    pub fn send(mut self, request: Request) -> AppResult<Response> {
        let mut bytes = serde_json::to_vec(&request)?;
        if bytes.len() >= MAX_MESSAGE {
            return Err(AppError::invalid("request exceeds one MiB"));
        }
        bytes.push(b'\n');
        self.stream.write_all(&bytes)?;
        let mut line = vec![];
        BufReader::new(&mut self.stream)
            .take((MAX_MESSAGE + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if line.len() > MAX_MESSAGE || line.last() != Some(&b'\n') {
            return Err(AppError::invalid(
                "incomplete or oversized service response",
            ));
        }
        let response: Response = serde_json::from_slice(&line)?;
        if response.protocol_version != PROTOCOL_VERSION
            || response.request_id != request.request_id
        {
            return Err(AppError::invalid(
                "service protocol or request identity mismatch",
            ));
        }
        Ok(response)
    }
}
