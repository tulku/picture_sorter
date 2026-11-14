use super::protocol::*;
use crate::{
    config::Config,
    error::{AppError, AppResult, ErrorCode},
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::MetadataExt, net::UnixStream},
    },
    time::Duration,
};
pub struct Client {
    stream: UnixStream,
}
impl Client {
    pub fn connect(config: &Config) -> AppResult<Self> {
        let state = crate::fs::RootHandle::open(&config.state_root, crate::fs::RootPolicy::Local)?;
        Self::connect_state(&state)
    }
    pub fn connect_with_timeout(config: &Config, timeout: Duration) -> AppResult<Self> {
        let state = crate::fs::RootHandle::open(&config.state_root, crate::fs::RootPolicy::Local)?;
        Self::connect_state_options(&state, Some(timeout))
    }
    pub(crate) fn connect_state(state: &crate::fs::RootHandle) -> AppResult<Self> {
        Self::connect_state_options(state, None)
    }
    fn connect_state_options(
        state: &crate::fs::RootHandle,
        timeout: Option<Duration>,
    ) -> AppResult<Self> {
        state.require_local()?;
        state.validate_identity()?;
        let socket = std::path::PathBuf::from(format!("/proc/self/fd/{}/service.sock", state.fd()));
        let metadata = std::fs::symlink_metadata(&socket)?;
        // Private socket ownership and kernel peer credentials must agree before sending a request.
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o600 {
            return Err(AppError::new(
                ErrorCode::Permission,
                "service socket must be owned by current user with mode 0600",
            ));
        }
        let stream = if timeout.is_some() {
            connect_without_waiting(&socket)?
        } else {
            UnixStream::connect(socket)?
        };
        super::server::check_peer(&stream)?;
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(Some(timeout.unwrap_or(Duration::from_secs(5))))?;
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

fn connect_without_waiting(path: &std::path::Path) -> std::io::Result<UnixStream> {
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() >= address.sun_path.len() || bytes.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid Unix socket path",
        ));
    }
    for (destination, source) in address.sun_path.iter_mut().zip(bytes) {
        *destination = *source as libc::c_char;
    }
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    let length =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    // A busy accept queue rejects this optional snapshot instead of blocking the foreground wait.
    let result = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let stream = UnixStream::from(socket);
    stream.set_nonblocking(false)?;
    Ok(stream)
}
