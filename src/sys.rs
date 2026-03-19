use std::io;
use std::os::unix::io::RawFd;

/// Set a file descriptor to non-blocking mode.
pub fn set_nonblock(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Set a file descriptor to blocking mode.
pub fn set_blocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Set close-on-exec on a file descriptor.
pub fn set_cloexec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Create a pipe with close-on-exec set on both ends.
pub fn pipe_cloexec() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [0i32; 2];

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    #[cfg(target_os = "macos")]
    {
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        for &fd in &fds {
            // SAFETY: fd is a valid file descriptor just created by pipe().
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
    }

    Ok((fds[0], fds[1]))
}

/// Install a signal handler that writes a byte to a pipe.
///
/// Returns the read end of the pipe. The signal handler writes to the write end.
/// The write end is stored in a static so the signal handler can access it.
pub fn signal_pipe(sig: libc::c_int) -> io::Result<RawFd> {
    let (read_fd, write_fd) = pipe_cloexec()?;
    set_nonblock(read_fd)?;
    set_nonblock(write_fd)?;

    // Store write_fd in a global so the signal handler can reach it.
    // We use sig as index into a small array.
    unsafe {
        let idx = sig as usize;
        if idx >= SIGNAL_WRITE_FDS.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "signal out of range",
            ));
        }
        SIGNAL_WRITE_FDS[idx].store(write_fd, std::sync::atomic::Ordering::Relaxed);

        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = signal_handler as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(read_fd)
}

#[allow(clippy::declare_interior_mutable_const)]
static SIGNAL_WRITE_FDS: [std::sync::atomic::AtomicI32; 32] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
    [INIT; 32]
};

extern "C" fn signal_handler(sig: libc::c_int) {
    let idx = sig as usize;
    if idx < SIGNAL_WRITE_FDS.len() {
        let fd = SIGNAL_WRITE_FDS[idx].load(std::sync::atomic::Ordering::Relaxed);
        if fd >= 0 {
            // SAFETY: writing a single byte to a pipe is async-signal-safe.
            unsafe {
                libc::write(fd, &1u8 as *const u8 as *const libc::c_void, 1);
            }
        }
    }
}

/// Get terminal size from a tty fd.
pub fn get_winsize(fd: RawFd) -> io::Result<(u32, u32)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ is a safe ioctl that reads terminal dimensions.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let sx = if ws.ws_col == 0 { 80 } else { ws.ws_col as u32 };
    let sy = if ws.ws_row == 0 { 24 } else { ws.ws_row as u32 };
    Ok((sx, sy))
}

/// Set terminal size on a pty fd.
pub fn set_winsize(fd: RawFd, sx: u32, sy: u32) -> io::Result<()> {
    let ws = libc::winsize {
        ws_row: sy as u16,
        ws_col: sx as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ is a safe ioctl that sets terminal dimensions.
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Block SIGCHLD, SIGWINCH, SIGTERM, SIGINT in the current thread.
/// Call before spawning the event loop so signals are handled via the signal pipe.
pub fn block_signals() {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGCHLD);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

/// Ignore SIGPIPE (broken pipe from dead client connections).
pub fn ignore_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// Write all bytes to a file descriptor, handling EINTR and partial writes.
/// Returns the total bytes written (may be less than `buf.len()` on error or EAGAIN).
pub fn write_all_fd(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    let mut written = 0;
    while written < buf.len() {
        // SAFETY: writing to a valid file descriptor.
        let n = unsafe {
            libc::write(
                fd,
                buf[written..].as_ptr() as *const libc::c_void,
                buf.len() - written,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == io::ErrorKind::WouldBlock && written > 0 {
                return Ok(written);
            }
            return Err(err);
        }
        written += n as usize;
    }
    Ok(written)
}

pub fn close_fd(fd: RawFd) {
    if fd >= 0 {
        // SAFETY: closing a valid file descriptor.
        unsafe {
            libc::close(fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipe_cloexec() {
        let (r, w) = pipe_cloexec().unwrap();
        assert!(r >= 0);
        assert!(w >= 0);
        assert_ne!(r, w);

        // Verify close-on-exec is set
        let flags_r = unsafe { libc::fcntl(r, libc::F_GETFD) };
        assert!(flags_r & libc::FD_CLOEXEC != 0);
        let flags_w = unsafe { libc::fcntl(w, libc::F_GETFD) };
        assert!(flags_w & libc::FD_CLOEXEC != 0);

        close_fd(r);
        close_fd(w);
    }

    #[test]
    fn test_set_nonblock() {
        let (r, w) = pipe_cloexec().unwrap();
        set_nonblock(r).unwrap();

        let flags = unsafe { libc::fcntl(r, libc::F_GETFL) };
        assert!(flags & libc::O_NONBLOCK != 0);

        close_fd(r);
        close_fd(w);
    }

    #[test]
    fn test_signal_pipe() {
        // Use SIGUSR1 to avoid interfering with other signal handlers
        let read_fd = signal_pipe(libc::SIGUSR1).unwrap();
        assert!(read_fd >= 0);

        // Send the signal to ourselves
        unsafe {
            libc::raise(libc::SIGUSR1);
        }

        // Should be able to read a byte from the pipe
        let mut buf = [0u8; 1];
        let n = unsafe { libc::read(read_fd, buf.as_mut_ptr() as *mut libc::c_void, 1) };
        assert_eq!(n, 1);

        close_fd(read_fd);
    }

    #[test]
    fn test_write_all_fd() {
        let (r, w) = pipe_cloexec().unwrap();
        let msg = b"hello world";
        let n = write_all_fd(w, msg).unwrap();
        assert_eq!(n, msg.len());

        let mut buf = [0u8; 32];
        let n = unsafe { libc::read(r, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        assert_eq!(n as usize, msg.len());
        assert_eq!(&buf[..msg.len()], msg);

        close_fd(r);
        close_fd(w);
    }
}
