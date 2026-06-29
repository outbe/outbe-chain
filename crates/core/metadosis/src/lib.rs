// Public surface (reached cross-crate): storage types, runtime entrypoints, the
// daily-accumulation sink, the precompile ABI, and shared constants.
pub mod constants;
pub mod daily_accumulation;
pub mod precompile;
pub mod runtime;
pub mod schema;

// Crate-internal: the two FSMs, local storage helpers, and domain errors. Their
// `pub` methods on `MetadosisContract` (a `schema` type) stay reachable; only the
// module paths are hidden.
mod errors;
mod metadosis;
mod state;
mod worldwideday;

#[cfg(test)]
mod tests;
