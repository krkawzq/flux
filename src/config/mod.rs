//! Configuration module - unified configuration management
//!
//! Directory structure:
//! - .flux/          Local workspace (created by `flux init`)
//! - ~/.flux/        Global management directory
//!
//! Provides:
//! - Configuration file discovery
//! - Variable placeholder resolution ({{var}} and {{var:default}})
//! - Interactive input for missing values
//! - Configuration inheritance

pub mod finder;
pub mod models;
pub mod resolver;

