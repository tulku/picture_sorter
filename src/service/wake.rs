use crate::error::AppResult;
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(crate) struct Notification(Arc<OwnedFd>);
impl Notification {
    pub fn new() -> AppResult<Self> {
        // eventfd returns an owned counter descriptor; nonblocking signals coalesce safely.
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // A successful eventfd call transfers ownership of the descriptor.
        Ok(Self(Arc::new(unsafe { OwnedFd::from_raw_fd(fd) })))
    }
    pub fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
    pub fn signal(&self) {
        let value: u64 = 1;
        // eventfd accepts exactly one native u64; a saturated counter is already readable.
        unsafe {
            libc::write(self.fd(), (&value as *const u64).cast(), 8);
        }
    }
    pub fn drain(&self) {
        let mut value: u64 = 0;
        // Reading a nonblocking eventfd atomically drains its accumulated counter.
        unsafe {
            libc::read(self.fd(), (&mut value as *mut u64).cast(), 8);
        }
    }
}

pub(crate) fn descriptor(fd: RawFd, events: libc::c_short) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}
pub(crate) fn wait(fds: &mut [libc::pollfd], timeout: Duration) -> AppResult<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let millis = remaining
            .as_millis()
            .saturating_add(u128::from(
                !remaining.subsec_nanos().is_multiple_of(1_000_000),
            ))
            .min(i32::MAX as u128) as i32;
        // poll borrows the live descriptor array for the duration of this blocking call.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
        if result >= 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
        if Instant::now() >= deadline {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notification_wakes_wait_and_drains_without_repeated_readiness() {
        let signal = Notification::new().unwrap();
        let sender = signal.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            sender.signal();
        });
        let mut fds = [descriptor(signal.fd(), libc::POLLIN)];
        wait(&mut fds, Duration::from_secs(2)).unwrap();
        assert_ne!(fds[0].revents & libc::POLLIN, 0);
        signal.drain();
        wait(&mut fds, Duration::ZERO).unwrap();
        assert_eq!(fds[0].revents, 0);
        worker.join().unwrap();
    }
    #[test]
    fn timeout_without_descriptors_sleeps_until_deadline() {
        let start = Instant::now();
        wait(&mut [], Duration::from_millis(20)).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(20));
    }
}
