//! CLI library — argv parsing, config loading, output formatting, and the
//! runtime that dispatches commands to a Subsonic client.
//!
//! The binary at `src/main.rs` is a thin wrapper that just calls `app::run`.

pub mod api;
pub mod app;
pub mod auth;
pub mod cli;
pub mod config;
pub mod format;
pub mod gateway;
pub mod playlist;
pub mod ratings;
pub mod recommend;
pub mod style;
pub mod sync;
pub mod tui;
