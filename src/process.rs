use std::{
    ops::{Deref, DerefMut},
    process::Child,
};
pub(crate) struct ChildGuard {
    child: Child,
    terminate_owned_group: bool,
}
impl ChildGuard {
    pub fn new(child: Child) -> Self {
        let worker = std::env::args_os()
            .nth(1)
            .is_some_and(|arg| arg == "--nas-worker" || arg == "--local-worker");
        // Only the isolated worker owns its process group and all writing descendants.
        let terminate_owned_group = worker && unsafe { libc::getpgrp() == libc::getpid() };
        Self {
            child,
            terminate_owned_group,
        }
    }
}
impl Deref for ChildGuard {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            if self.terminate_owned_group {
                // No failure receipt may outlive an uncertain writer in the worker's dedicated group.
                unsafe {
                    libc::kill(0, libc::SIGKILL);
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_error_stops_and_reaps_local_child_before_returning() {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        drop(ChildGuard::new(child));
        // kill with signal zero observes existence without sending a signal.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
