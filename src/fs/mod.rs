mod atomic;
mod roots;
mod space;
pub use crate::domain::ArchivePath;
pub use atomic::*;
pub use roots::*;
pub use space::*;
pub fn path_from_os(value: &std::ffi::OsStr) -> crate::error::AppResult<ArchivePath> {
    ArchivePath::parse(value.to_str().ok_or_else(|| {
        crate::error::AppError::invalid("non-UTF-8 filename; rename it before management")
    })?)
}
