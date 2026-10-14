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
    fn eq(&self, other: &&str) -> bool {
        let mut rest = Remaining(other);
        std::fmt::Write::write_fmt(&mut rest, format_args!("{self}")).is_ok() && rest.0.is_empty()
    }
}

/// Consumes the expected text as the error is formatted, without allocating.
struct Remaining<'a>(&'a str);

impl std::fmt::Write for Remaining<'_> {
    fn write_str(&mut self, piece: &str) -> std::fmt::Result {
        self.0 = self.0.strip_prefix(piece).ok_or(std::fmt::Error)?;
        Ok(())
    }
}

impl From<QuoteParseError> for String {
    fn from(error: QuoteParseError) -> Self {
        error.to_string()
    }
}
