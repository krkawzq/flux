//! SSH client module

mod client;

pub use client::{create_client, SshClient, SshClientTrait, SshConfig};
