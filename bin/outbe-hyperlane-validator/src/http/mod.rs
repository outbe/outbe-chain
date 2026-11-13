//! HTTP servers of the daemon.
//!
//! - [`checkpoints`]: the S3-compatible read endpoint for relayers
//!   (default `0.0.0.0:9000`).
//! - [`health`]: `GET /health` and `GET /status` (default `0.0.0.0:9003`).

pub mod checkpoints;
pub mod health;
