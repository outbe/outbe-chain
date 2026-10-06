//! Promisfactory precompile (`0x2337`). Thin orchestration layer on top of the
//! Promis token (`outbe_promis`, `0x1337`).
//!
//! Owns Promis mint and burn orchestration. `runtime::mint` delegates to
//! `outbe_promis::api::mint`.
//! `mine_coen` burns Promis through `outbe_promis::api::burn` and issues matching native COEN.
//! `mine_gratis` converts Promis into Gratis through GratisFactory.

pub mod api;
pub mod precompile;
pub mod runtime;

#[cfg(test)]
mod tests;
