//! Gateway between music clients and the upstream Navidrome (Subsonic) server.
//!
//! Public surface is intentionally small:
//! - [`Config`] — TOML-parsed configuration.
//! - [`AppState`] — shared runtime state (bearer token, upstream creds).
//! - [`build_router`] — constructs the axum router for both production and tests.

pub mod admin;
pub mod app;
pub mod auth;
pub mod auto_projection;
pub mod config;
pub mod diagnostics;
pub mod embedder;
pub mod events;
pub mod ingest;
pub mod oauth;
pub mod proxy;
pub mod recommend;
pub mod recommend_feedback;
pub mod scrobble;
pub mod state;
pub mod sync;

pub use app::build_router;
pub use config::Config;
pub use state::AppState;
