use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

#[cfg(unix)]
pub(crate) fn open_no_follow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
pub(crate) fn open_no_follow(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
pub(crate) fn is_symlink_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
pub(crate) fn is_symlink_error(_error: &io::Error) -> bool {
    false
}

pub(crate) fn read_limited(file: &File, max_bytes: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds size limit",
        ));
    }
    Ok(bytes)
}

pub(crate) struct DirLock {
    _file: File,
}

impl DirLock {
    pub(crate) fn acquire(parent: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(parent.join(".pqr-index.lock"))?;
        #[cfg(unix)]
        {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self { _file: file })
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            unsafe {
                libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}
