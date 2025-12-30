//! Shell execution module
//!
//! Provides cross-platform local shell script execution capabilities.
//! Supports multiple backends with automatic fallback.

mod executor;
mod system_backend;

pub use executor::{ShellBackend, ShellExecutor, ShellOutput};

/// Create a new shell executor with default backends
pub fn create_executor() -> ShellExecutor {
    ShellExecutor::new()
}
