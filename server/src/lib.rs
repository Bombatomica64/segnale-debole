//! Segnale Debole backend: daily / time-trial leaderboards, Authentik OIDC
//! login and cloud profile sync. See `main.rs` for the binary.

pub mod api;
pub mod config;
pub mod merge;
pub mod oidc;
pub mod pg;
pub mod ratelimit;
pub mod store;
pub mod validate;

pub use api::{AppState, router};
