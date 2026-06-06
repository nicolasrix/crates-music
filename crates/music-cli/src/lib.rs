//! CLI library — argv parsing, config loading, output formatting, and the
//! runtime that dispatches commands to a Subsonic client.
//!
//! The binary at `src/main.rs` is a thin wrapper that just calls `app::run`.

pub mod app;
pub mod cli;
pub mod config;
pub mod format;
pub mod gateway;
pub mod ratings;
pub mod recommend;
pub mod sync;
