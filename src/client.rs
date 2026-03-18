use std::io;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixStream;

use anyhow::{Context, Result};
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};

use crate::protocol::{self, Message};
use crate::sys;

const TOKEN_SOCKET: Token = Token(0);
const TOKEN_TTY: Token = Token(1);
const TOKEN_SIGWINCH: Token = Token(2);

pub(crate) fn run_client(
    msg_type: u16,
    session_name: &str,
) -> Result<()> {
    let socket_path = protocol::socket_path();

    // Connect to server
    let stream = UnixStream::connect(&socket_path)
        .with_context(|| format!("connecting to {}", socket_path.display()))?;
    let sock_fd = {
        use std::os::unix::io::AsRawFd;
        stream.as_raw_fd()
    };

    sys::set_nonblock(sock_fd)?;

    let tty_fd = open_tty()?;
    let (sx, sy) = sys::get_winsize(tty_fd)?;

    // Send the client's tty fd to the server
    protocol::send_fd(sock_fd, tty_fd)?;

    // Send identify message
    let payload = protocol::encode_identify(session_name, sx, sy);
    let msg = Message::new(msg_type, payload);
    send_blocking(sock_fd, &msg.encode())?;

    // For list command, just read the response and print it
    if msg_type == protocol::MSG_LIST {
        return handle_list_response(sock_fd);
    }

    // For kill command, wait for response
    if msg_type == protocol::MSG_KILL_SESSION {
        return handle_kill_response(sock_fd);
    }

    // Enter raw mode
    let saved_termios = enter_raw_mode(tty_fd)?;

    // Set up signal handling for SIGWINCH
    let sigwinch_fd = sys::signal_pipe(libc::SIGWINCH)?;

    // Set up mio event loop
    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(64);

    poll.registry().register(
        &mut SourceFd(&sock_fd),
        TOKEN_SOCKET,
        Interest::READABLE,
    )?;
    poll.registry().register(
        &mut SourceFd(&tty_fd),
        TOKEN_TTY,
        Interest::READABLE,
    )?;
    poll.registry().register(
        &mut SourceFd(&sigwinch_fd),
        TOKEN_SIGWINCH,
        Interest::READABLE,
    )?;

    let result = client_loop(&mut poll, &mut events, sock_fd, tty_fd, sigwinch_fd);

    // Restore terminal
    restore_terminal(tty_fd, &saved_termios);

    result
}

fn client_loop(
    poll: &mut Poll,
    events: &mut Events,
    sock_fd: RawFd,
    tty_fd: RawFd,
    sigwinch_fd: RawFd,
) -> Result<()> {
    let mut recv_buf = Vec::new();
    let mut read_buf = [0u8; 8192];

    loop {
        poll.poll(events, None)?;

        for event in events.iter() {
            match event.token() {
                TOKEN_SOCKET => {
                    // Data from server (or disconnect)
                    let n = unsafe {
                        libc::read(
                            sock_fd,
                            read_buf.as_mut_ptr() as *mut libc::c_void,
                            read_buf.len(),
                        )
                    };
                    if n <= 0 {
                        // Server disconnected
                        return Ok(());
                    }
                    recv_buf.extend_from_slice(&read_buf[..n as usize]);

                    // Process complete messages
                    while let Some((msg, consumed)) = Message::decode(&recv_buf) {
                        recv_buf.drain(..consumed);
                        match msg.msg_type {
                            protocol::MSG_EXIT | protocol::MSG_DETACH => {
                                return Ok(());
                            }
                            protocol::MSG_ERROR => {
                                let err = String::from_utf8_lossy(&msg.payload);
                                anyhow::bail!("{err}");
                            }
                            _ => {}
                        }
                    }
                }
                TOKEN_TTY => {
                    // Input from terminal — forward to server
                    let n = unsafe {
                        libc::read(
                            tty_fd,
                            read_buf.as_mut_ptr() as *mut libc::c_void,
                            read_buf.len(),
                        )
                    };
                    if n <= 0 {
                        return Ok(());
                    }
                    let data = &read_buf[..n as usize];
                    let msg = Message::new(protocol::MSG_INPUT, data.to_vec());
                    let _ = send_nonblocking(sock_fd, &msg.encode());
                }
                TOKEN_SIGWINCH => {
                    // Drain signal pipe
                    let mut drain = [0u8; 64];
                    unsafe {
                        libc::read(
                            sigwinch_fd,
                            drain.as_mut_ptr() as *mut libc::c_void,
                            drain.len(),
                        );
                    }
                    // Get new size and send to server
                    if let Ok((sx, sy)) = sys::get_winsize(tty_fd) {
                        let payload = protocol::encode_resize(sx, sy);
                        let msg = Message::new(protocol::MSG_RESIZE, payload);
                        let _ = send_nonblocking(sock_fd, &msg.encode());
                    }
                }
                _ => {}
            }
        }
    }
}

fn handle_list_response(sock_fd: RawFd) -> Result<()> {
    let mut recv_buf = Vec::new();
    let mut read_buf = [0u8; 4096];

    loop {
        let n = unsafe {
            libc::read(
                sock_fd,
                read_buf.as_mut_ptr() as *mut libc::c_void,
                read_buf.len(),
            )
        };
        if n <= 0 {
            break;
        }
        recv_buf.extend_from_slice(&read_buf[..n as usize]);

        while let Some((msg, consumed)) = Message::decode(&recv_buf) {
            recv_buf.drain(..consumed);
            if msg.msg_type == protocol::MSG_LIST_RESPONSE {
                let text = String::from_utf8_lossy(&msg.payload);
                print!("{text}");
                return Ok(());
            }
            if msg.msg_type == protocol::MSG_ERROR {
                let err = String::from_utf8_lossy(&msg.payload);
                anyhow::bail!("{err}");
            }
        }
    }
    Ok(())
}

fn handle_kill_response(sock_fd: RawFd) -> Result<()> {
    let mut recv_buf = Vec::new();
    let mut read_buf = [0u8; 4096];

    loop {
        let n = unsafe {
            libc::read(
                sock_fd,
                read_buf.as_mut_ptr() as *mut libc::c_void,
                read_buf.len(),
            )
        };
        if n <= 0 {
            break;
        }
        recv_buf.extend_from_slice(&read_buf[..n as usize]);

        while let Some((msg, consumed)) = Message::decode(&recv_buf) {
            recv_buf.drain(..consumed);
            match msg.msg_type {
                protocol::MSG_EXIT => return Ok(()),
                protocol::MSG_ERROR => {
                    let err = String::from_utf8_lossy(&msg.payload);
                    anyhow::bail!("{err}");
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn open_tty() -> io::Result<RawFd> {
    let fd = unsafe {
        libc::open(
            c"/dev/tty".as_ptr(),
            libc::O_RDWR,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    sys::set_cloexec(fd)?;
    Ok(fd)
}

fn enter_raw_mode(fd: RawFd) -> io::Result<libc::termios> {
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let mut raw = saved;
    unsafe {
        libc::cfmakeraw(&mut raw);
    }
    // Disable echoing, canonical mode, signals
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;

    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(saved)
}

fn restore_terminal(fd: RawFd, saved: &libc::termios) {
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, saved);
    }
}

fn send_blocking(fd: RawFd, data: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written < data.len() {
        let n = unsafe {
            libc::write(
                fd,
                data[written..].as_ptr() as *const libc::c_void,
                data.len() - written,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == io::ErrorKind::WouldBlock {
                // Busy-wait for writable (simple client)
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            return Err(err);
        }
        written += n as usize;
    }
    Ok(())
}

fn send_nonblocking(fd: RawFd, data: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written < data.len() {
        let n = unsafe {
            libc::write(
                fd,
                data[written..].as_ptr() as *const libc::c_void,
                data.len() - written,
            )
        };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(err);
        }
        written += n as usize;
    }
    Ok(())
}
