use serde_json::{json, Map, Value};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReferenceFailure {
    kind: &'static str,
    ordinal: Option<usize>,
    currency: Option<u16>,
}

impl ReferenceFailure {
    pub(crate) const fn new(kind: &'static str) -> Self {
        Self {
            kind,
            ordinal: None,
            currency: None,
        }
    }

    pub(crate) const fn ordinal(kind: &'static str, ordinal: usize) -> Self {
        Self {
            kind,
            ordinal: Some(ordinal),
            currency: None,
        }
    }

    pub(crate) const fn ordinal_currency(
        kind: &'static str,
        ordinal: usize,
        currency: u16,
    ) -> Self {
        Self {
            kind,
            ordinal: Some(ordinal),
            currency: Some(currency),
        }
    }

    pub(crate) fn envelope(&self) -> Value {
        let mut error = Map::new();
        error.insert("kind".to_owned(), Value::String(self.kind.to_owned()));
        if let Some(ordinal) = self.ordinal {
            error.insert("ordinal".to_owned(), json!(ordinal));
        }
        if let Some(currency) = self.currency {
            error.insert("currency".to_owned(), json!(currency));
        }
        json!({"status": "FAILURE", "error": error})
    }
}
