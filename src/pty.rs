use std::ffi::CString;
use std::io;
use std::os::unix::io::RawFd;
use std::path::Path;

use crate::sys;

/// Spawn a child process in a new PTY. Returns (master_fd, child_pid).
pub fn spawn_shell(
    sx: u32,
    sy: u32,
    cwd: Option<&str>,
    socket_path: &Path,
    server_pid: u32,
    pane_id: u32,
) -> io::Result<(RawFd, libc::pid_t)> {
    let ws = libc::winsize {
        ws_row: sy as u16,
        ws_col: sx as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let mut master: RawFd = -1;
    // SAFETY: forkpty is a standard POSIX function that creates a new PTY pair.
    let pid = unsafe {
        libc::forkpty(
            &mut master,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &ws as *const libc::winsize as *mut libc::winsize,
        )
    };

    if pid < 0 {
        return Err(io::Error::last_os_error());
    }

    if pid == 0 {
        // Child process
        child_exec(cwd, socket_path, server_pid, pane_id);
    }

    // Parent
    sys::set_nonblock(master)?;
    sys::set_cloexec(master)?;

    Ok((master, pid))
}

fn child_exec(cwd: Option<&str>, socket_path: &Path, server_pid: u32, pane_id: u32) -> ! {
    // Set environment variables
    let tm_val = format!("{},{},{}", socket_path.display(), server_pid, pane_id);

    unsafe {
        let key = CString::new("TERM").unwrap();
        let val = CString::new("tmux-256color").unwrap();
        libc::setenv(key.as_ptr(), val.as_ptr(), 1);

        let key = CString::new("TM").unwrap();
        let val = CString::new(tm_val.as_str()).unwrap();
        libc::setenv(key.as_ptr(), val.as_ptr(), 1);

        let key = CString::new("TMUX").unwrap();
        libc::setenv(key.as_ptr(), val.as_ptr(), 1);
    }

    // Change directory
    if let Some(dir) = cwd
        && let Ok(dir) = CString::new(dir)
    {
        unsafe {
            libc::chdir(dir.as_ptr());
        }
    }

    // Get shell
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let shell_name = Path::new(&shell)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let login_name = format!("-{shell_name}");

    let shell_c = CString::new(shell.as_str()).unwrap();
    let argv0 = CString::new(login_name.as_str()).unwrap();

    // SAFETY: execl replaces the process image with the shell.
    unsafe {
        libc::execl(
            shell_c.as_ptr(),
            argv0.as_ptr(),
            std::ptr::null::<libc::c_char>(),
        );
        // If execl returns, it failed
        libc::_exit(127);
    }
}
