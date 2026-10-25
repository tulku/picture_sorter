use super::RootHandle;
use crate::error::{AppError, AppResult};
pub fn available_bytes(root: &RootHandle) -> AppResult<u64> {
    root.validate_identity()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // fstatvfs fills the structure on success using the held filesystem descriptor.
    if unsafe { libc::fstatvfs(root.fd(), st.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // The successful syscall initialized the structure.
    let st = unsafe { st.assume_init() };
    st.f_bavail
        .checked_mul(st.f_frsize)
        .ok_or_else(|| AppError::invalid("free-space count overflow"))
}
