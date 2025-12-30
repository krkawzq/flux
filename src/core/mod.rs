//! Core module - fundamental infrastructure
//!
//! This module provides core functionality used across all other modules:
//! - Error types and Result alias
//! - SSH client wrapper
//! - Platform-specific abstractions
//! - Configuration constants

pub mod config;
pub mod error;
pub mod platform;
pub mod ssh;

pub use config::Config;
pub use error::{RemoteError, Result};
