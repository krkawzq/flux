//! Flux library — internal crate exposed for tests.
//!
//! Modules are progressively opened as Phase 2 tasks add them. The bin
//! (`src/main.rs`) currently uses module-private declarations and does NOT
//! depend on this crate's public API yet.
//!
//! Roadmap (Phase 2):
//!   pub mod cli;        // Task 12 (cli/mod.rs)
//!   pub mod config;     // Task 13 (config/mod.rs)
//!   pub mod remote;     // Tasks 2-4
//!   pub mod reporter;   // Task 6
//!   pub mod sync;       // Task 5+ (sync::plan, then file/script/block/mod)

pub mod audit;
pub mod cli;
pub mod config;
pub mod path;
pub mod remote;
pub mod reporter;
pub mod sync;
