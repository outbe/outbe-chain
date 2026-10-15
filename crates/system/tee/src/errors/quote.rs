//! Typed errors for SGX quote measurement parsing.

use thiserror::Error;

/// Errors returned by [`crate::quote::parse_quote_measurements`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum QuoteParseError {
    /// Quote byte slice is shorter than the minimum SGX report body length.
    #[error("quote too short: {actual} < {min}")]
    TooShort {
        /// Actual byte length of the quote slice.
        actual: usize,
        /// Minimum required byte length.
        min: usize,
    },
}

impl PartialEq<&str> for QuoteParseError {
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &&str) -> bool {
        self.to_string() == *other
    }
}

impl From<QuoteParseError> for String {
    fn from(error: QuoteParseError) -> Self {
        error.to_string()
    }
}
