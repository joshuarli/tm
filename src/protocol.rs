use std::io;
use std::os::unix::io::RawFd;

// Message types (u16)
pub(crate) const MSG_IDENTIFY: u16 = 1;
pub(crate) const MSG_INPUT: u16 = 2;
pub(crate) const MSG_RESIZE: u16 = 3;
pub(crate) const MSG_DETACH: u16 = 4;
pub(crate) const MSG_NEW_SESSION: u16 = 10;
pub(crate) const MSG_ATTACH: u16 = 11;
pub(crate) const MSG_LIST: u16 = 12;
pub(crate) const MSG_LIST_RESPONSE: u16 = 13;
pub(crate) const MSG_KILL_SESSION: u16 = 14;
pub(crate) const MSG_EXIT: u16 = 15;
pub(crate) const MSG_ERROR: u16 = 16;

/// Wire format: [u32 length][u16 type][payload]
/// length includes the type field but not itself.
pub(crate) struct Message {
    pub(crate) msg_type: u16,
    pub(crate) payload: Vec<u8>,
}

impl Message {
    pub(crate) fn new(msg_type: u16, payload: Vec<u8>) -> Self {
        Self { msg_type, payload }
    }

    pub(crate) fn empty(msg_type: u16) -> Self {
        Self {
            msg_type,
            payload: Vec::new(),
        }
    }

    /// Serialize to wire format.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let len = (2 + self.payload.len()) as u32;
        let mut buf = Vec::with_capacity(4 + len as usize);
        buf.extend_from_slice(&len.to_ne_bytes());
        buf.extend_from_slice(&self.msg_type.to_ne_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Try to decode a message from a buffer. Returns (message, bytes_consumed) or None.
    pub(crate) fn decode(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.len() < 4 {
            return None;
        }
        let len = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if len < 2 {
            return None;
        }
        if buf.len() < 4 + len {
            return None;
        }
        let msg_type = u16::from_ne_bytes([buf[4], buf[5]]);
        let payload = buf[6..4 + len].to_vec();
        Some((Self { msg_type, payload }, 4 + len))
    }
}

// Identify message payload: session_name (null-terminated string) + sx(u32) + sy(u32)
pub(crate) fn encode_identify(session_name: &str, sx: u32, sy: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(session_name.as_bytes());
    buf.push(0);
    buf.extend_from_slice(&sx.to_ne_bytes());
    buf.extend_from_slice(&sy.to_ne_bytes());
    buf
}

pub(crate) fn decode_identify(payload: &[u8]) -> Option<(String, u32, u32)> {
    let nul = payload.iter().position(|&b| b == 0)?;
    let name = String::from_utf8_lossy(&payload[..nul]).into_owned();
    let rest = &payload[nul + 1..];
    if rest.len() < 8 {
        return None;
    }
    let sx = u32::from_ne_bytes([rest[0], rest[1], rest[2], rest[3]]);
    let sy = u32::from_ne_bytes([rest[4], rest[5], rest[6], rest[7]]);
    Some((name, sx, sy))
}

pub(crate) fn encode_resize(sx: u32, sy: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(8);
    buf.extend_from_slice(&sx.to_ne_bytes());
    buf.extend_from_slice(&sy.to_ne_bytes());
    buf
}

pub(crate) fn decode_resize(payload: &[u8]) -> Option<(u32, u32)> {
    if payload.len() < 8 {
        return None;
    }
    let sx = u32::from_ne_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let sy = u32::from_ne_bytes([payload[4], payload[5], payload[6], payload[7]]);
    Some((sx, sy))
}

/// Properly aligned cmsg buffer for SCM_RIGHTS (one fd).
#[repr(C)]
struct CmsgFd {
    hdr: libc::cmsghdr,
    fd: libc::c_int,
}

/// Send a file descriptor over a Unix socket using SCM_RIGHTS.
pub(crate) fn send_fd(sock: RawFd, fd: RawFd) -> io::Result<()> {
    use std::mem;

    let mut data = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr() as *mut libc::c_void,
        iov_len: 1,
    };

    let cmsg_space = unsafe { libc::CMSG_SPACE(mem::size_of::<libc::c_int>() as u32) };

    // Use the aligned struct as our cmsg buffer
    let mut cmsg_buf = unsafe { mem::zeroed::<CmsgFd>() };

    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = &mut cmsg_buf as *mut CmsgFd as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;

    // SAFETY: cmsg_buf is properly aligned and sized for one fd.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(mem::size_of::<libc::c_int>() as u32) as _;
        std::ptr::copy_nonoverlapping(
            &fd as *const libc::c_int as *const u8,
            libc::CMSG_DATA(cmsg),
            mem::size_of::<libc::c_int>(),
        );

        loop {
            let r = libc::sendmsg(sock, &msg, 0);
            if r < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            break;
        }
    }
    Ok(())
}

/// Receive an optional file descriptor from a Unix socket using SCM_RIGHTS.
/// Returns Ok(Some(fd)) if an fd was sent, Ok(None) if just a plain byte.
pub(crate) fn recv_fd(sock: RawFd) -> io::Result<Option<RawFd>> {
    use std::mem;

    let mut data = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr() as *mut libc::c_void,
        iov_len: 1,
    };

    let cmsg_space = unsafe { libc::CMSG_SPACE(mem::size_of::<libc::c_int>() as u32) };

    let mut cmsg_buf = unsafe { mem::zeroed::<CmsgFd>() };

    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = &mut cmsg_buf as *mut CmsgFd as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;

    // SAFETY: cmsg_buf is properly aligned and sized.
    unsafe {
        let ret = loop {
            let r = libc::recvmsg(sock, &mut msg, 0);
            if r < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            break r;
        };
        if ret == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
        }

        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Ok(None); // no fd attached — non-interactive client
        }
        if (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
            return Ok(None);
        }

        let mut fd: libc::c_int = 0;
        std::ptr::copy_nonoverlapping(
            libc::CMSG_DATA(cmsg),
            &mut fd as *mut libc::c_int as *mut u8,
            mem::size_of::<libc::c_int>(),
        );
        Ok(Some(fd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_roundtrip() {
        let msg = Message::new(MSG_INPUT, b"hello".to_vec());
        let encoded = msg.encode();
        let (decoded, consumed) = Message::decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.msg_type, MSG_INPUT);
        assert_eq!(decoded.payload, b"hello");
    }

    #[test]
    fn test_message_empty() {
        let msg = Message::empty(MSG_DETACH);
        let encoded = msg.encode();
        let (decoded, consumed) = Message::decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.msg_type, MSG_DETACH);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn test_identify_roundtrip() {
        let payload = encode_identify("test-session", 120, 40);
        let (name, sx, sy) = decode_identify(&payload).unwrap();
        assert_eq!(name, "test-session");
        assert_eq!(sx, 120);
        assert_eq!(sy, 40);
    }

    #[test]
    fn test_resize_roundtrip() {
        let payload = encode_resize(200, 50);
        let (sx, sy) = decode_resize(&payload).unwrap();
        assert_eq!(sx, 200);
        assert_eq!(sy, 50);
    }

    #[test]
    fn test_partial_decode() {
        let msg = Message::new(MSG_INPUT, b"test".to_vec());
        let encoded = msg.encode();
        // Only give partial data
        assert!(Message::decode(&encoded[..3]).is_none());
        assert!(Message::decode(&encoded[..5]).is_none());
        // Full data works
        assert!(Message::decode(&encoded).is_some());
    }

    #[test]
    fn test_multiple_messages_in_buffer() {
        let msg1 = Message::new(MSG_INPUT, b"ab".to_vec());
        let msg2 = Message::new(MSG_RESIZE, encode_resize(80, 24));
        let mut buf = msg1.encode();
        buf.extend_from_slice(&msg2.encode());

        let (decoded1, consumed1) = Message::decode(&buf).unwrap();
        assert_eq!(decoded1.msg_type, MSG_INPUT);
        assert_eq!(decoded1.payload, b"ab");

        let (decoded2, consumed2) = Message::decode(&buf[consumed1..]).unwrap();
        assert_eq!(decoded2.msg_type, MSG_RESIZE);
        assert_eq!(consumed1 + consumed2, buf.len());
    }

    #[test]
    fn test_send_recv_fd() {
        // Create a Unix socket pair and pass an fd through it
        let mut fds = [0i32; 2];
        let ret = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(ret, 0);

        // Create a pipe — we'll pass the read end through the socket
        let mut pipe_fds = [0i32; 2];
        let ret = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(ret, 0);

        // Write something to the pipe so we can verify the received fd works
        let data = b"hello";
        unsafe {
            libc::write(pipe_fds[1], data.as_ptr() as *const libc::c_void, data.len());
        }

        // Send the pipe read end through the socket
        send_fd(fds[0], pipe_fds[0]).expect("send_fd");

        // Receive it on the other end
        let received_fd = recv_fd(fds[1]).expect("recv_fd").expect("should have fd");
        assert!(received_fd >= 0);
        assert_ne!(received_fd, pipe_fds[0]); // must be a new fd number

        // Read from the received fd — should get "hello"
        let mut buf = [0u8; 16];
        let n = unsafe { libc::read(received_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"hello");

        // Clean up
        for fd in [fds[0], fds[1], pipe_fds[0], pipe_fds[1], received_fd] {
            unsafe { libc::close(fd); }
        }
    }

    #[test]
    fn test_cmsg_alignment() {
        // Verify our CmsgFd struct matches CMSG_SPACE
        let cmsg_space = unsafe {
            libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as u32)
        } as usize;
        assert!(std::mem::size_of::<CmsgFd>() >= cmsg_space);
        assert!(std::mem::align_of::<CmsgFd>() >= std::mem::align_of::<libc::cmsghdr>());
    }
}

/// Get the socket path.
pub(crate) fn socket_path() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return std::path::PathBuf::from(dir).join("tm/default");
    }
    if let Some(dir) = std::env::var_os("TMPDIR") {
        let uid = unsafe { libc::getuid() };
        return std::path::PathBuf::from(dir).join(format!("tm-{uid}/default"));
    }
    let uid = unsafe { libc::getuid() };
    std::path::PathBuf::from(format!("/tmp/tm-{uid}/default"))
}
