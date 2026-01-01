//! SSH client module

mod client;
pub mod config;

pub use client::{create_client, SshClient, SshClientTrait, SshConfig};
pub use config::{append_ssh_config, SshConfigEntry};
