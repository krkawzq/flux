//! Unix-specific platform implementation

use super::BackgroundService;
use crate::core::config::{get_logs_dir, get_state_dir};
use crate::core::error::{RemoteError, Result};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Unix background service implementation using daemonize
pub struct UnixBackgroundService {
    state_dir: PathBuf,
    logs_dir: PathBuf,
}

impl UnixBackgroundService {
    pub fn new() -> Self {
        let state_dir = get_state_dir();
        let logs_dir = get_logs_dir();

        // Ensure directories exist
        let _ = fs::create_dir_all(&state_dir);
        let _ = fs::create_dir_all(&logs_dir);

        Self {
            state_dir,
            logs_dir,
        }
    }

    fn pid_file(&self, name: &str) -> PathBuf {
        self.state_dir.join(format!("{}.pid", name))
    }
}

impl BackgroundService for UnixBackgroundService {
    fn spawn_background(&self, name: &str, args: Vec<String>) -> Result<u32> {
        use nix::sys::signal::{self, Signal};
        use nix::unistd::{fork, setsid, ForkResult};
        use std::os::unix::process::CommandExt;

        let log_file = self.log_path(name);
        let error_log_file = self.error_log_path(name);
        let pid_file = self.pid_file(name);

        // Fork the process
        match unsafe { fork() } {
            Ok(ForkResult::Parent { child }) => {
                // Parent: save PID and return
                let pid = child.as_raw() as u32;
                fs::write(&pid_file, pid.to_string()).map_err(|e| {
                    RemoteError::Platform(format!("Failed to write PID file: {}", e))
                })?;
                Ok(pid)
            }
            Ok(ForkResult::Child) => {
                // Child: become session leader and exec
                let _ = setsid();

                // Get current executable
                let exe = std::env::current_exe().map_err(|e| {
                    RemoteError::Platform(format!("Failed to get executable: {}", e))
                })?;

                // Open log files
                let stdout_file = fs::File::create(&log_file).map_err(|e| {
                    RemoteError::Platform(format!("Failed to create log file: {}", e))
                })?;
                let stderr_file = fs::File::create(&error_log_file).map_err(|e| {
                    RemoteError::Platform(format!("Failed to create error log: {}", e))
                })?;

                // Execute the command
                let err = Command::new(exe)
                    .args(&args)
                    .stdin(Stdio::null())
                    .stdout(stdout_file)
                    .stderr(stderr_file)
                    .exec();

                // If exec returns, it failed
                eprintln!("Failed to exec: {:?}", err);
                std::process::exit(1);
            }
            Err(e) => Err(RemoteError::Platform(format!("Fork failed: {}", e))),
        }
    }

    fn stop_background(&self, pid: u32) -> Result<()> {
        use nix::sys::signal::{kill, Signal};
        use nix::unistd::Pid;

        let pid = Pid::from_raw(pid as i32);

        // Try SIGTERM first
        if let Err(e) = kill(pid, Signal::SIGTERM) {
            if e == nix::errno::Errno::ESRCH {
                // Process doesn't exist
                return Ok(());
            }
            return Err(RemoteError::Platform(format!(
                "Failed to send SIGTERM: {}",
                e
            )));
        }

        // Wait a bit
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Check if still running and force kill if needed
        if self.is_running(pid.as_raw() as u32) {
            let _ = kill(pid, Signal::SIGKILL);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(())
    }

    fn is_running(&self, pid: u32) -> bool {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;

        let pid = Pid::from_raw(pid as i32);
        kill(pid, None).is_ok()
    }

    fn log_path(&self, name: &str) -> PathBuf {
        self.logs_dir.join(format!("{}.log", name))
    }

    fn error_log_path(&self, name: &str) -> PathBuf {
        self.logs_dir.join(format!("{}.err", name))
    }
}
