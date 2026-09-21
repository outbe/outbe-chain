//! Operator-selected offline checks; no node startup or recovery integration.

pub(crate) mod bodies;
pub(crate) mod canonical_state;
pub(crate) mod ce;
pub(crate) mod evm;
pub(crate) mod headers;
pub(crate) mod ocomp;

/// Required native input cannot establish the requested complete observation.
#[derive(Debug)]
pub(crate) struct Incomplete(pub String);

impl std::fmt::Display for Incomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "incomplete: {}", self.0)
    }
}

impl std::error::Error for Incomplete {}
