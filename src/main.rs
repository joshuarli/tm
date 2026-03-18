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

use anyhow::{bail, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let cmd = args.get(1).map(String::as_str).unwrap_or("new");

    match cmd {
        "new" => {
            let session_name = parse_session_name(&args, 2);
            start_or_connect(protocol::MSG_NEW_SESSION, &session_name)
        }
        "attach" | "a" => {
            let session_name = parse_session_name(&args, 2);
            client::run_client(protocol::MSG_ATTACH, &session_name)
        }
        "ls" | "list" => {
            client::run_client(protocol::MSG_LIST, "")
        }
        "kill" => {
            let session_name = parse_session_name(&args, 2);
            client::run_client(protocol::MSG_KILL_SESSION, &session_name)
        }
        other => {
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

fn start_or_connect(msg_type: u16, session_name: &str) -> Result<()> {
    let socket_path = protocol::socket_path();

    // Check if server is already running
    if socket_path.exists() {
        // Try to connect
        match std::os::unix::net::UnixStream::connect(&socket_path) {
            Ok(_stream) => {
                drop(_stream);
                // Server exists — connect as client
                return client::run_client(msg_type, session_name);
            }
            Err(_) => {
                // Stale socket — remove it
                let _ = std::fs::remove_file(&socket_path);
            }
        }
    }

    // Fork: child becomes server, parent becomes client
    // SAFETY: fork is safe here before we've spawned any threads.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!("fork failed: {}", std::io::Error::last_os_error());
    }

    if pid == 0 {
        // Child — become server
        // Detach from controlling terminal
        unsafe {
            libc::setsid();
        }
        // Close stdin/stdout/stderr
        unsafe {
            libc::close(0);
            libc::close(1);
            libc::close(2);
            // Reopen as /dev/null
            libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            libc::dup2(0, 1);
            libc::dup2(0, 2);
        }

        if let Err(e) = server::run_server() {
            crate::log::_log(&format!("server error: {e:#}"));
        }
        std::process::exit(0);
    }

    // Parent — become client
    // Wait briefly for server to start
    for _ in 0..50 {
        if socket_path.exists() {
            if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    client::run_client(msg_type, session_name)
}
