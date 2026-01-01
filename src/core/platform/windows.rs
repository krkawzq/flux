//! Windows-specific platform implementation

use super::BackgroundService;
use crate::core::config::{get_logs_dir, get_state_dir};
use crate::core::error::{RemoteError, Result};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Windows background service implementation
pub struct WindowsBackgroundService {
    state_dir: PathBuf,
    logs_dir: PathBuf,
}

impl WindowsBackgroundService {
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

impl BackgroundService for WindowsBackgroundService {
    fn spawn_background(&self, name: &str, args: Vec<String>) -> Result<u32> {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const DETACHED_PROCESS: u32 = 0x00000008;

        let log_file = self.log_path(name);
        let error_log_file = self.error_log_path(name);
        let pid_file = self.pid_file(name);

        // Get current executable
        let exe = std::env::current_exe()
            .map_err(|e| RemoteError::Platform(format!("Failed to get executable: {}", e)))?;

        // Open log files
        let stdout_file = fs::File::create(&log_file)
            .map_err(|e| RemoteError::Platform(format!("Failed to create log file: {}", e)))?;
        let stderr_file = fs::File::create(&error_log_file)
            .map_err(|e| RemoteError::Platform(format!("Failed to create error log: {}", e)))?;

        // Spawn the process detached
        let child = Command::new(exe)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(stdout_file)
            .stderr(stderr_file)
            .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
            .spawn()
            .map_err(|e| RemoteError::Platform(format!("Failed to spawn process: {}", e)))?;

        let pid = child.id();

        // Save PID
        fs::write(&pid_file, pid.to_string())
            .map_err(|e| RemoteError::Platform(format!("Failed to write PID file: {}", e)))?;

        Ok(pid)
    }

    fn stop_background(&self, pid: u32) -> Result<()> {
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::processthreadsapi::{OpenProcess, TerminateProcess};
        use winapi::um::synchapi::WaitForSingleObject;
        
        use winapi::um::winnt::{PROCESS_TERMINATE, SYNCHRONIZE};

        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE | SYNCHRONIZE, 0, pid);
            if handle.is_null() {
                // Process doesn't exist or can't be accessed
                return Ok(());
            }

            // Terminate the process
            let result = TerminateProcess(handle, 1);
            if result == 0 {
                CloseHandle(handle);
                return Err(RemoteError::Platform(
                    "Failed to terminate process".to_string(),
                ));
            }

            // Wait briefly for termination
            WaitForSingleObject(handle, 1000);
            CloseHandle(handle);
        }

        Ok(())
    }

    fn is_running(&self, pid: u32) -> bool {
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::processthreadsapi::OpenProcess;
        use winapi::um::synchapi::WaitForSingleObject;
        use winapi::um::winnt::SYNCHRONIZE;

        unsafe {
            let handle = OpenProcess(SYNCHRONIZE, 0, pid);
            if handle.is_null() {
                return false;
            }

            // Check if process has exited (timeout 0 = immediate check)
            let result = WaitForSingleObject(handle, 0);
            CloseHandle(handle);

            // WAIT_TIMEOUT (258) means still running
            result == 258
        }
    }

    fn log_path(&self, name: &str) -> PathBuf {
        self.logs_dir.join(format!("{}.log", name))
    }

    fn error_log_path(&self, name: &str) -> PathBuf {
        self.logs_dir.join(format!("{}.err", name))
    }
}
