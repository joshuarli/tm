#![allow(dead_code)]

mod client;
mod config;
mod copy;
mod grid;
mod key_bind;
mod keys;
mod layout;
mod log;
mod prompt;
mod protocol;
mod pty;
mod render;
mod screen;
mod server;
mod state;
mod sys;
mod tty;
mod vt;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let cmd = args.get(1).map(String::as_str);

    match cmd {
        None | Some("-h") | Some("--help") | Some("help") => {
            println!("Usage:");
            println!("  tm new [-s NAME]      Create session and attach");
            println!("  tm attach [-t NAME]   Attach to session");
            println!("  tm ls                 List sessions");
            println!("  tm kill [-t NAME]     Kill session");
            Ok(())
        }
        Some("new") => {
            let session_name = parse_session_name(&args, 2);
            start_or_connect(protocol::MSG_NEW_SESSION, &session_name)
        }
        Some("attach" | "a") => {
            let session_name = parse_session_name(&args, 2);
            connect_or_fail(protocol::MSG_ATTACH, &session_name)
        }
        Some("ls" | "list") => {
            connect_or_fail(protocol::MSG_LIST, "")
        }
        Some("kill") => {
            let session_name = parse_session_name(&args, 2);
            connect_or_fail(protocol::MSG_KILL_SESSION, &session_name)
        }
        Some(other) => {
            bail!("unknown command: {other}\n\nUsage:\n  tm new [-s NAME]\n  tm attach [-t NAME]\n  tm ls\n  tm kill [-t NAME]");
        }
    }
}

fn parse_session_name(args: &[String], start: usize) -> String {
    let mut i = start;
    while i < args.len() {
        match args[i].as_str() {
            "-s" | "-t" => {
                if let Some(name) = args.get(i + 1) {
                    return name.clone();
                }
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }
    String::new()
}

/// Connect to an existing server. Fails if no server is running.
fn connect_or_fail(msg_type: u16, session_name: &str) -> Result<()> {
    client::run_client(msg_type, session_name)
}

/// Connect to existing server, or fork a new one.
/// Uses a socketpair so the initial client and server can communicate
/// immediately — no startup race condition.
fn start_or_connect(msg_type: u16, session_name: &str) -> Result<()> {
    let socket_path = protocol::socket_path();

    // If server socket exists, try to use it directly
    if socket_path.exists() {
        match client::run_client(msg_type, session_name) {
            Ok(()) => return Ok(()),
            Err(_) => {
                // Server is dead — remove stale socket and start fresh
                let _ = std::fs::remove_file(&socket_path);
            }
        }
    }

    // Create a socketpair — one end for the parent (client), one for the child (server).
    // This eliminates the race between server bind and client connect.
    let mut pair = [0i32; 2];
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, pair.as_mut_ptr()) } != 0 {
        bail!("socketpair failed: {}", std::io::Error::last_os_error());
    }

    // SAFETY: fork is safe here before we've spawned any threads.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!("fork failed: {}", std::io::Error::last_os_error());
    }

    if pid == 0 {
        // Child — become server
        let server_end = pair[1];
        sys::close_fd(pair[0]); // close parent's end

        // Redirect stdin/stdout/stderr to /dev/null
        unsafe {
            let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            if devnull >= 0 {
                libc::dup2(devnull, 0);
                libc::dup2(devnull, 1);
                libc::dup2(devnull, 2);
                if devnull > 2 {
                    libc::close(devnull);
                }
            }
        }

        crate::log::init();

        // Pass the socketpair fd to the server — it becomes the first client connection
        if let Err(e) = server::run_server_with_client(server_end) {
            crate::log::_log(&format!("server error: {e:#}"));
        }
        std::process::exit(0);
    }

    // Parent — become client using the socketpair
    let client_end = pair[0];
    sys::close_fd(pair[1]); // close child's end

    client::run_client_on_fd(msg_type, session_name, client_end)
}
