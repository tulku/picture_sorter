use crate::error::{AppError, AppResult};
use rusqlite::ffi;
use std::{
    ffi::{CStr, c_char, c_int},
    sync::OnceLock,
};
static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
unsafe extern "C" fn full_path(
    _: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    length: c_int,
    output: *mut c_char,
) -> c_int {
    if name.is_null() || output.is_null() || length <= 0 {
        return ffi::SQLITE_CANTOPEN;
    }
    // SQLite supplies a terminated input string and a writable output buffer of length bytes.
    let bytes = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul();
    if bytes.len() > length as usize || !bytes.starts_with(b"/proc/self/fd/") {
        return ffi::SQLITE_CANTOPEN;
    }
    // Retain the held descriptor path instead of unix VFS canonicalization through procfs.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), output, bytes.len());
    }
    ffi::SQLITE_OK
}
pub fn register() -> AppResult<()> {
    let result = REGISTERED.get_or_init(|| {
        // The default unix VFS and its callbacks have process lifetime after SQLite initialization.
        let base = unsafe { ffi::sqlite3_vfs_find(std::ptr::null()) };
        if base.is_null() {
            return Err("SQLite unix VFS unavailable".into());
        }
        // Copy the VFS dispatch table while retaining its existing process-lifetime context.
        let mut vfs = Box::new(unsafe { std::ptr::read(base) });
        vfs.zName = c"photo-archive-fd".as_ptr();
        vfs.xFullPathname = Some(full_path);
        vfs.pNext = std::ptr::null_mut();
        let vfs = Box::leak(vfs);
        // Registration retains this leaked immutable dispatch table for the process lifetime.
        let rc = unsafe { ffi::sqlite3_vfs_register(vfs, 0) };
        if rc == ffi::SQLITE_OK {
            Ok(())
        } else {
            Err(format!("SQLite VFS registration failed: {rc}"))
        }
    });
    result.as_ref().map(|_| ()).map_err(AppError::integrity)
}
