//! Structured telemetry fields that preserve complete error chains.

use core::{error::Error, fmt};

/// A borrowed error and all of its source errors, formatted without allocation.
pub struct ErrorChain<'a, E: ?Sized>(&'a E);

impl<E: Error + ?Sized> fmt::Display for ErrorChain<'_, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut cur = self.0.source();
        while let Some(src) = cur {
            write!(f, ": {src}")?;
            cur = src.source();
        }
        Ok(())
    }
}

/// Format an error's full source chain as a structured tracing field.
pub fn error_field<E: Error + ?Sized>(err: &E) -> tracing::field::DisplayValue<ErrorChain<'_, E>> {
    tracing::field::display(ErrorChain(err))
}

#[cfg(test)]
mod tests {
    use super::error_field;

    #[derive(Debug, thiserror::Error)]
    #[error("{label}")]
    struct Failure {
        label: &'static str,
        #[source]
        cause: Option<Box<Failure>>,
    }

    #[test]
    fn formats_single_error() {
        let error = Failure {
            label: "root",
            cause: None,
        };
        assert_eq!(error_field(&error).to_string(), "root");
    }

    #[test]
    fn formats_three_level_source_chain() {
        let error = Failure {
            label: "outer",
            cause: Some(Box::new(Failure {
                label: "middle",
                cause: Some(Box::new(Failure {
                    label: "root",
                    cause: None,
                })),
            })),
        };
        let error: &dyn core::error::Error = &error;
        assert_eq!(error_field(error).to_string(), "outer: middle: root");
    }
}
