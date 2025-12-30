//! SSH client module

mod client;

pub use client::{create_client, AuthMethod, ExecResult, SshClient, SshClientTrait, SshConfig};
