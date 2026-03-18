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

/// Send a file descriptor over a Unix socket using SCM_RIGHTS.
pub(crate) fn send_fd(sock: RawFd, fd: RawFd) -> io::Result<()> {
    use std::mem;

    let data = [0u8; 1];
    let iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: 1,
    };

    // cmsg buffer must be properly aligned
    #[repr(C)]
    struct CmsgBuf {
        hdr: libc::cmsghdr,
        fd: libc::c_int,
    }
    let _cmsg_buf = unsafe { mem::zeroed::<CmsgBuf>() };

    let cmsg_len = unsafe { libc::CMSG_LEN(mem::size_of::<libc::c_int>() as u32) } as usize;
    let cmsg_space = unsafe { libc::CMSG_SPACE(mem::size_of::<libc::c_int>() as u32) } as usize;

    // Use a raw byte buffer for the cmsg to ensure proper alignment
    let mut cmsg_bytes = vec![0u8; cmsg_space];

    let msg = libc::msghdr {
        msg_name: std::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &iov as *const libc::iovec as *mut libc::iovec,
        msg_iovlen: 1,
        msg_control: cmsg_bytes.as_mut_ptr() as *mut libc::c_void,
        msg_controllen: cmsg_space as _,
        msg_flags: 0,
    };

    // SAFETY: the cmsg buffer is properly sized and aligned.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = cmsg_len as _;
        std::ptr::copy_nonoverlapping(
            &fd as *const libc::c_int as *const u8,
            libc::CMSG_DATA(cmsg),
            mem::size_of::<libc::c_int>(),
        );

        let ret = libc::sendmsg(sock, &msg, 0);
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Receive a file descriptor from a Unix socket using SCM_RIGHTS.
pub(crate) fn recv_fd(sock: RawFd) -> io::Result<RawFd> {
    use std::mem;

    let mut data = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr() as *mut libc::c_void,
        iov_len: 1,
    };

    let cmsg_space = unsafe { libc::CMSG_SPACE(mem::size_of::<libc::c_int>() as u32) } as usize;
    let mut cmsg_bytes = vec![0u8; cmsg_space];

    let mut msg = libc::msghdr {
        msg_name: std::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: cmsg_bytes.as_mut_ptr() as *mut libc::c_void,
        msg_controllen: cmsg_space as _,
        msg_flags: 0,
    };

    // SAFETY: the cmsg buffer is properly sized.
    unsafe {
        let ret = libc::recvmsg(sock, &mut msg, 0);
        if ret < 0 {
            return Err(io::Error::last_os_error());
        }
        if ret == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
        }

        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "no cmsg"));
        }
        if (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected cmsg type"));
        }

        let mut fd: libc::c_int = 0;
        std::ptr::copy_nonoverlapping(
            libc::CMSG_DATA(cmsg),
            &mut fd as *mut libc::c_int as *mut u8,
            mem::size_of::<libc::c_int>(),
        );
        Ok(fd)
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
