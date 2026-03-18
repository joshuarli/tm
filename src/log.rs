use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);

pub(crate) fn init() {
    if std::env::var_os("TM_LOG").is_none() {
        return;
    }
    let path = log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
        *LOG_FILE.lock().unwrap() = Some(f);
    }
}

fn log_path() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return std::path::PathBuf::from(dir).join("tm/tm.log");
    }
    if let Some(dir) = std::env::var_os("TMPDIR") {
        let uid = unsafe { libc::getuid() };
        return std::path::PathBuf::from(dir).join(format!("tm-{uid}/tm.log"));
    }
    let uid = unsafe { libc::getuid() };
    std::path::PathBuf::from(format!("/tmp/tm-{uid}/tm.log"))
}

#[allow(unused_macros)]
macro_rules! tm_log {
    ($($arg:tt)*) => {
        $crate::log::_log(&format!($($arg)*))
    };
}

#[allow(unused_imports)]
pub(crate) use tm_log;

pub(crate) fn _log(msg: &str) {
    if let Ok(mut guard) = LOG_FILE.lock() {
        if let Some(f) = guard.as_mut() {
            let _ = writeln!(f, "{msg}");
        }
    }
}
