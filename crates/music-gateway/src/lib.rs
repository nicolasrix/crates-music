//! Gateway between music clients and the upstream Navidrome (Subsonic) server.
//!
//! Public surface is intentionally small:
//! - [`Config`] — TOML-parsed configuration.
//! - [`AppState`] — shared runtime state (bearer token, upstream creds).
//! - [`build_router`] — constructs the axum router for both production and tests.

pub mod app;
pub mod auth;
pub mod config;
pub mod proxy;
pub mod state;

pub use app::build_router;
pub use config::Config;
pub use state::AppState;
