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

    // If server socket exists, try to use it directly — no probe connection
    if socket_path.exists() {
        match client::run_client(msg_type, session_name) {
            Ok(()) => return Ok(()),
            Err(_) => {
                // Server is dead — remove stale socket and start fresh
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
        // Redirect stdin/stdout/stderr to /dev/null, but do NOT setsid —
        // the server needs the controlling terminal to write to client tty fds.
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

        match server::run_server() {
            Ok(()) => {}
            Err(e) => {
                let msg = format!("server error: {e:#}");
                crate::log::_log(&msg);
                let crash_path = socket_path.with_file_name("crash.log");
                let _ = std::fs::write(&crash_path, msg.as_bytes());
            }
        }
        std::process::exit(0);
    }

    // Parent — wait for server socket to appear
    for _ in 0..200 {
        if socket_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    client::run_client(msg_type, session_name)
}
