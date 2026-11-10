//! Crash diagnostics: the program has no console, so a panic or an abort used to vanish without a trace.
//! - a panic hook appends the message, a backtrace and the last log lines to `crash.log`
//! - a runaway allocation (an endless loop eating memory) is reported the same way before the abort
//! - `running.flag` exists while the program runs: if it is still there at the next start, the previous
//!   session ended abnormally (even when no hook could run) and the user is told so.
//! Everything lives in `%APPDATA%\DSO-510-Link`.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static NOTES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static BUSY: AtomicBool = AtomicBool::new(false);
const NOTES_KEPT: usize = 200;

pub fn dir() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("DSO-510-Link")
}

pub fn log_path() -> PathBuf {
    dir().join("crash.log")
}

fn flag_path() -> PathBuf {
    dir().join("running.flag")
}

/// Remember a line for the crash report (the last 200 are kept).
pub fn note(line: &str) {
    if let Ok(mut n) = NOTES.lock() {
        if n.len() >= NOTES_KEPT {
            n.pop_front();
        }
        n.push_back(format!("{} {line}", crate::export::stamp()));
    }
}

/// Append a report to crash.log.
pub fn write_report(what: &str) {
    let _ = std::fs::create_dir_all(dir());
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) else { return };
    let _ = writeln!(f, "==== {} UTC  DSO-510 Link {} ====", crate::export::stamp(), env!("CARGO_PKG_VERSION"));
    let _ = writeln!(f, "{what}");
    let _ = writeln!(f, "backtrace:\n{}", std::backtrace::Backtrace::force_capture());
    if let Ok(n) = NOTES.lock() {
        let _ = writeln!(f, "last log lines:");
        for l in n.iter() {
            let _ = writeln!(f, "  {l}");
        }
    }
    let _ = writeln!(f);
}

#[cfg(windows)]
fn message_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    unsafe { MessageBoxW(std::ptr::null_mut(), w(text).as_ptr(), w("DSO-510 Link").as_ptr(), MB_OK | MB_ICONERROR) };
}

#[cfg(not(windows))]
fn message_box(_: &str) {}

/// Panics are written to crash.log and announced; the default hook (stderr) still runs.
pub fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        write_report(&format!("panic in thread '{}': {info}", thread.name().unwrap_or("?")));
        default(info);
        message_box(&format!("The program hit an internal error and has to close.\n\nDetails were saved to:\n{}", log_path().display()));
    }));
}

/// A single allocation far beyond anything the program needs: report and abort (called by the allocator guard).
pub fn huge_allocation(bytes: usize) -> ! {
    if !BUSY.swap(true, Ordering::SeqCst) {
        write_report(&format!("runaway memory use: a request for {bytes} bytes (an endless loop?)"));
        message_box(&format!("The program ran out of memory and has to close.\n\nDetails were saved to:\n{}", log_path().display()));
    }
    std::process::abort()
}

/// Call at start-up. True if the previous session did not end normally.
pub fn begin_session() -> bool {
    let _ = std::fs::create_dir_all(dir());
    let crashed = flag_path().exists();
    let _ = std::fs::write(flag_path(), crate::export::stamp());
    crashed
}

/// Call on a normal exit.
pub fn end_session() {
    let _ = std::fs::remove_file(flag_path());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_and_session_flag() {
        let tmp = std::env::temp_dir().join(format!("dso510-crashlog-test-{}", std::process::id()));
        // SAFETY: the only test touching the environment
        unsafe { std::env::set_var("APPDATA", &tmp) };
        assert!(!begin_session(), "clean start");
        assert!(begin_session(), "the flag of an unfinished session is seen");
        end_session();
        assert!(!begin_session(), "a normal exit removes the flag");
        end_session();
        note("hello from the test");
        write_report("something went wrong");
        let text = std::fs::read_to_string(log_path()).unwrap();
        assert!(text.contains("something went wrong") && text.contains("hello from the test") && text.contains("backtrace"), "{text}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
