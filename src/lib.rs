//! Flux - SSH Remote Server Management Tool
//!
//! A powerful tool for managing remote servers with:
//! - Configuration synchronization
//! - SSH reverse proxy tunnels
//! - File transfer capabilities
//!
//! ## Directory Structure
//!
//! - `.flux/` - Local workspace directory (created by `flux init`)
//! - `~/.flux/` - Global configuration directory

// Allow dead code during development - these functions will be used as the project matures
#![allow(dead_code)]

pub mod cli;
pub mod config;
pub mod core;
pub mod proxy;
pub mod shell;
pub mod state;
pub mod sync;
