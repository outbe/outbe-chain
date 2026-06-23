//! Gratisfactory precompile (`0x2003`). Thin orchestration layer on top of
//! the shielded gratis pool (`outbe_gratispool`, `0x2004`).

pub mod api;
pub mod errors;
pub mod precompile;
pub mod runtime;

#[cfg(test)]
mod tests;
