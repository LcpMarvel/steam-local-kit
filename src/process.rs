//! Whether the Steam client (or any process the caller names) is running,
//! and asking Steam to exit and start the way Steam itself does. Nothing
//! here kills a process: a shutdown is a request Steam may take its time
//! over, and a probe that cannot tell says `Unknown`, which never counts as
//! stopped.

#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{error::IoOperation, SteamError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessStatus {
    Running,
    Stopped,
    Unknown,
}

/// The Steam client's process names on this platform.
#[cfg(windows)]
pub const STEAM_PROCESS_NAMES: &[&str] = &["steam.exe"];
#[cfg(target_os = "macos")]
pub const STEAM_PROCESS_NAMES: &[&str] = &["steam_osx"];
#[cfg(all(unix, not(target_os = "macos")))]
pub const STEAM_PROCESS_NAMES: &[&str] = &["steam"];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// Whether a process with one of these exact image names runs, compared
/// case-insensitively on Windows (`dota2.exe`) and exactly elsewhere.
#[cfg(windows)]
pub fn process_status(names: &[&str]) -> ProcessStatus {
    let output = match Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return ProcessStatus::Unknown,
    };
    // One CSV row per process, the image name first.
    let listing = String::from_utf8_lossy(&output.stdout);
    let running = listing.lines().any(|line| {
        line.split(',').next().is_some_and(|image| {
            let image = image.trim_matches('"');
            names.iter().any(|name| image.eq_ignore_ascii_case(name))
        })
    });
    if running {
        ProcessStatus::Running
    } else {
        ProcessStatus::Stopped
    }
}

#[cfg(not(windows))]
pub fn process_status(names: &[&str]) -> ProcessStatus {
    let mut unknown = false;
    for name in names {
        match Command::new("pgrep").args(["-x", name]).output() {
            Ok(output) if output.status.success() => return ProcessStatus::Running,
            // pgrep exits 1 when nothing matched.
            Ok(output) if output.status.code() == Some(1) => {}
            _ => unknown = true,
        }
    }
    if unknown {
        ProcessStatus::Unknown
    } else {
        ProcessStatus::Stopped
    }
}

pub fn steam_status() -> ProcessStatus {
    process_status(STEAM_PROCESS_NAMES)
}

/// Asks the running Steam client to exit as its own menu would, so it
/// saves its configuration first. Returns once the request is handed over;
/// use [`wait_for_steam_exit`] to know Steam is gone.
///
/// `steam_root` is the client's install root, where `steam.exe` lives on
/// Windows; macOS and Linux reach Steam through the system instead.
pub fn request_steam_shutdown(steam_root: &Path) -> Result<(), SteamError> {
    #[cfg(windows)]
    let (program, args) = (steam_executable(steam_root)?, vec!["-shutdown"]);
    #[cfg(target_os = "macos")]
    let (program, args) = {
        let _ = steam_root;
        (PathBuf::from("open"), vec!["-g", "steam://exit"])
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let (program, args) = {
        let _ = steam_root;
        (PathBuf::from("steam"), vec!["-shutdown"])
    };
    spawn(&program, &args)
}

/// Waits until no Steam client process is left, probing twice a second.
pub fn wait_for_steam_exit(timeout: Duration) -> Result<(), SteamError> {
    let started = Instant::now();
    loop {
        match steam_status() {
            ProcessStatus::Stopped => return Ok(()),
            ProcessStatus::Unknown => return Err(SteamError::ProcessProbeFailed),
            ProcessStatus::Running if started.elapsed() >= timeout => {
                return Err(SteamError::SteamStillRunning { waited: timeout })
            }
            ProcessStatus::Running => thread::sleep(Duration::from_millis(500)),
        }
    }
}

/// Starts the Steam client and returns without waiting for it.
pub fn start_steam(steam_root: &Path) -> Result<(), SteamError> {
    #[cfg(windows)]
    let (program, args) = (steam_executable(steam_root)?, Vec::new());
    #[cfg(target_os = "macos")]
    let (program, args) = {
        let _ = steam_root;
        (PathBuf::from("open"), vec!["-g", "-a", "Steam"])
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let (program, args) = {
        let _ = steam_root;
        (PathBuf::from("steam"), Vec::new())
    };
    spawn(&program, &args)
}

#[cfg(windows)]
fn steam_executable(steam_root: &Path) -> Result<PathBuf, SteamError> {
    let path = steam_root.join("steam.exe");
    if path.is_file() {
        Ok(path)
    } else {
        Err(SteamError::io(
            IoOperation::Launch,
            path,
            std::io::Error::from(std::io::ErrorKind::NotFound),
        ))
    }
}

fn spawn(program: &Path, args: &[&str]) -> Result<(), SteamError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(DETACHED_PROCESS);
    let mut child = command
        .spawn()
        .map_err(|error| SteamError::io(IoOperation::Launch, program.to_path_buf(), error))?;
    // Reap it whenever it ends, so a long-lived Steam leaves no zombie.
    thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{process_status, ProcessStatus};

    #[test]
    fn a_process_that_is_not_running_reads_as_stopped() {
        // A broken probe would say Unknown instead.
        assert_eq!(process_status(&["slk-no-such"]), ProcessStatus::Stopped);
    }
}
