use crate::vm::error::Result;
use crate::vm::exception::pending_helpers::set_pending_io_exception;
use crate::vm::helper::get_handle;
use crate::vm::safepoint;
use crate::vm::system_native::platform_file::PlatformFile;
use nix::sys::stat::fstat;
use nix::sys::uio::pread;
use nix::unistd::{ftruncate, read, write};
use std::os::fd::BorrowedFd;
use std::slice::{from_raw_parts, from_raw_parts_mut};

const IOS_EOF: i32 = -1;
const IOS_UNAVAILABLE: i32 = -2;
const IOS_INTERRUPTED: i32 = -3;

/// `sun.nio.ch.UnixFileDispatcherImpl.write0(Ljava/io/FileDescriptor;JI)I`
pub(crate) fn write0(fd_ref: i32, address: i64, len: i32) -> Result<i32> {
    let address = address as usize as *const u8;
    let buf = unsafe { from_raw_parts(address, len as usize) };

    let fd = get_handle(fd_ref)?;
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    // Can block on a pipe, socket or terminal; only native memory is touched while Safe
    let result = match safepoint::blocking(|| write(fd, buf)) {
        Ok(written) => written as i32,
        Err(errno) => match errno {
            nix::errno::Errno::EAGAIN => IOS_UNAVAILABLE,
            nix::errno::Errno::EINTR => IOS_INTERRUPTED,
            _ => {
                set_pending_io_exception(&format!("Write failed: {}", errno))?;
                return Ok(0);
            }
        },
    };

    Ok(result)
}

/// `sun.nio.ch.UnixFileDispatcherImpl.read0(Ljava/io/FileDescriptor;JI)I`
pub(crate) fn read0(fd_ref: i32, address: i64, len: i32) -> Result<i32> {
    let address = address as usize as *mut u8;
    let buf = unsafe { from_raw_parts_mut(address, len as usize) };

    let fd = (get_handle(fd_ref))?;
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    // Can block on a pipe, socket or terminal; only native memory is touched while Safe
    let result = match safepoint::blocking(|| read(fd, buf)) {
        Ok(read) => {
            if read == 0 {
                IOS_EOF // EOF is -1 in javaland
            } else {
                read as i32
            }
        }
        Err(errno) => match errno {
            nix::errno::Errno::EAGAIN => IOS_UNAVAILABLE,
            nix::errno::Errno::EINTR => IOS_INTERRUPTED,
            _ => {
                set_pending_io_exception(&format!("Read failed: {}", errno))?;
                return Ok(0);
            }
        },
    };

    Ok(result)
}

/// `sun.nio.ch.UnixFileDispatcherImpl.pread0(Ljava/io/FileDescriptor;JIJ)I`
pub(crate) fn pread0(fd_ref: i32, address: i64, len: i32, position: i64) -> Result<i32> {
    let address = address as usize as *mut u8;
    let buf = unsafe { from_raw_parts_mut(address, len as usize) };

    let fd = (get_handle(fd_ref))?;
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let result = match safepoint::blocking(|| pread(fd, buf, position)) {
        Ok(read) => {
            if read == 0 {
                IOS_EOF // EOF is -1 in javaland
            } else {
                read as i32
            }
        }
        Err(errno) => match errno {
            nix::errno::Errno::EAGAIN => IOS_UNAVAILABLE,
            nix::errno::Errno::EINTR => IOS_INTERRUPTED,
            _ => {
                set_pending_io_exception(&format!("Read failed: {}", errno))?;
                return Ok(0);
            }
        },
    };

    Ok(result)
}

/// `sun.nio.ch.UnixFileDispatcherImpl.size0(Ljava/io/FileDescriptor;)J`
pub(crate) fn size0(fd_ref: i32) -> Result<i64> {
    let fd = (get_handle(fd_ref))?;
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let result = match fstat(fd) {
        Ok(stat) => stat.st_size,
        Err(errno) if matches!(errno, nix::errno::Errno::EINTR) => IOS_INTERRUPTED as i64,
        Err(errno) => {
            set_pending_io_exception(&format!("Size failed: {}", errno))?;
            return Ok(0);
        }
    };

    Ok(result)
}

/// `sun.nio.ch.UnixFileDispatcherImpl.truncate0(Ljava/io/FileDescriptor;J)I`
pub(crate) fn truncate0(fd_ref: i32, size: i64) -> Result<i32> {
    let Some(file) = PlatformFile::get_by_fd(fd_ref)? else {
        return Ok(0);
    };

    let ret = match ftruncate(&*file, size) {
        Ok(_) => 0,
        Err(errno) if matches!(errno, nix::errno::Errno::EINTR) => IOS_INTERRUPTED,
        Err(errno) => {
            set_pending_io_exception(&format!("Truncate failed: {}", errno))?;
            return Ok(0);
        }
    };

    Ok(ret)
}

fn last_errno() -> nix::errno::Errno {
    nix::errno::Errno::last()
}

/// Maps a failed `ssize_t` syscall like the JDK's `convertReturnVal`/`convertLongReturnVal`.
fn convert_failure(what: &str) -> Result<i64> {
    match last_errno() {
        nix::errno::Errno::EAGAIN => Ok(IOS_UNAVAILABLE as i64),
        nix::errno::Errno::EINTR => Ok(IOS_INTERRUPTED as i64),
        errno => {
            set_pending_io_exception(&format!("{what} failed: {errno}"))?;
            Ok(0)
        }
    }
}

/// `sun.nio.ch.UnixFileDispatcherImpl.available0(Ljava/io/FileDescriptor;)I`
pub(crate) fn available0(fd_ref: i32) -> Result<i32> {
    use nix::libc;
    let fd = get_handle(fd_ref)?;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let mut size: i64 = -1;
    if unsafe { libc::fstat(fd, &mut stat) } >= 0 {
        let kind = stat.st_mode & libc::S_IFMT;
        if kind == libc::S_IFCHR || kind == libc::S_IFIFO || kind == libc::S_IFSOCK {
            let mut n: libc::c_int = 0;
            if unsafe { libc::ioctl(fd, libc::FIONREAD as _, &mut n) } >= 0 {
                return Ok(n);
            }
        } else if kind == libc::S_IFREG {
            size = stat.st_size as i64;
        }
    }
    let current = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) } as i64;
    if current == -1 {
        set_pending_io_exception(&format!("Available failed: {}", last_errno()))?;
        return Ok(0);
    }
    if size < current {
        size = unsafe { libc::lseek(fd, 0, libc::SEEK_END) } as i64;
        if size == -1 || unsafe { libc::lseek(fd, current as libc::off_t, libc::SEEK_SET) } == -1 {
            set_pending_io_exception(&format!("Available failed: {}", last_errno()))?;
            return Ok(0);
        }
    }
    Ok((size - current).clamp(0, i32::MAX as i64) as i32)
}

/// `sun.nio.ch.UnixFileDispatcherImpl.pwrite0(Ljava/io/FileDescriptor;JIJ)I`
pub(crate) fn pwrite0(fd_ref: i32, address: i64, len: i32, position: i64) -> Result<i32> {
    use nix::libc;
    let fd = get_handle(fd_ref)?;
    let (buf, len) = (address as usize, len as usize);
    let written = safepoint::blocking(|| unsafe {
        libc::pwrite(fd, buf as *const libc::c_void, len, position as libc::off_t)
    });
    if written < 0 {
        return Ok(convert_failure("Write")? as i32);
    }
    Ok(written as i32)
}

/// `sun.nio.ch.UnixFileDispatcherImpl.readv0(Ljava/io/FileDescriptor;JI)J`
pub(crate) fn readv0(fd_ref: i32, address: i64, count: i32) -> Result<i64> {
    use nix::libc;
    let fd = get_handle(fd_ref)?;
    let iov = address as usize;
    let read =
        safepoint::blocking(|| unsafe { libc::readv(fd, iov as *const libc::iovec, count) });
    if read < 0 {
        return convert_failure("Read");
    }
    Ok(if read == 0 {
        IOS_EOF as i64
    } else {
        read as i64
    })
}

/// `sun.nio.ch.UnixFileDispatcherImpl.writev0(Ljava/io/FileDescriptor;JI)J`
pub(crate) fn writev0(fd_ref: i32, address: i64, count: i32) -> Result<i64> {
    use nix::libc;
    let fd = get_handle(fd_ref)?;
    let iov = address as usize;
    let written =
        safepoint::blocking(|| unsafe { libc::writev(fd, iov as *const libc::iovec, count) });
    if written < 0 {
        return convert_failure("Write");
    }
    Ok(written as i64)
}

/// `sun.nio.ch.UnixFileDispatcherImpl.force0(Ljava/io/FileDescriptor;Z)I`
pub(crate) fn force0(fd_ref: i32, metadata: bool) -> Result<i32> {
    use nix::libc;
    let fd = get_handle(fd_ref)?;
    #[cfg(target_os = "macos")]
    let result = {
        let _ = metadata;
        safepoint::blocking(|| unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) })
    };
    #[cfg(not(target_os = "macos"))]
    let result = if metadata {
        safepoint::blocking(|| unsafe { libc::fsync(fd) })
    } else {
        safepoint::blocking(|| unsafe { libc::fdatasync(fd) })
    };
    if result < 0 {
        return Ok(convert_failure("Force")? as i32);
    }
    Ok(result)
}

const NO_LOCK: i32 = -1;
const LOCKED: i32 = 0;
const INTERRUPTED: i32 = 2;

fn flock_for(pos: i64, size: i64, kind: i32) -> nix::libc::flock {
    let mut lock: nix::libc::flock = unsafe { std::mem::zeroed() };
    lock.l_whence = nix::libc::SEEK_SET as _;
    lock.l_start = pos as _;
    // `Long.MAX_VALUE` means "to the end of the file", which POSIX spells as length 0.
    lock.l_len = if size == i64::MAX { 0 } else { size as _ };
    lock.l_type = kind as _;
    lock
}

/// `sun.nio.ch.UnixFileDispatcherImpl.lock0(Ljava/io/FileDescriptor;ZJJZ)I`
pub(crate) fn lock0(fd_ref: i32, block: bool, pos: i64, size: i64, shared: bool) -> Result<i32> {
    use nix::libc;
    let fd = get_handle(fd_ref)?;
    let kind = if shared { libc::F_RDLCK } else { libc::F_WRLCK };
    let lock = flock_for(pos, size, kind as i32);
    let command = if block { libc::F_SETLKW } else { libc::F_SETLK };
    let result = safepoint::blocking(|| unsafe { libc::fcntl(fd, command, &lock) });
    if result == -1 {
        let errno = last_errno();
        if !block && (errno == nix::errno::Errno::EAGAIN || errno == nix::errno::Errno::EACCES) {
            return Ok(NO_LOCK);
        }
        if errno == nix::errno::Errno::EINTR {
            return Ok(INTERRUPTED);
        }
        set_pending_io_exception(&format!("Lock failed: {errno}"))?;
        return Ok(NO_LOCK);
    }
    Ok(LOCKED)
}

/// `sun.nio.ch.UnixFileDispatcherImpl.release0(Ljava/io/FileDescriptor;JJ)V`
pub(crate) fn release0(fd_ref: i32, pos: i64, size: i64) -> Result<()> {
    use nix::libc;
    let fd = get_handle(fd_ref)?;
    let lock = flock_for(pos, size, libc::F_UNLCK as i32);
    if unsafe { libc::fcntl(fd, libc::F_SETLK, &lock) } == -1 {
        set_pending_io_exception(&format!("Release failed: {}", last_errno()))?;
    }
    Ok(())
}
