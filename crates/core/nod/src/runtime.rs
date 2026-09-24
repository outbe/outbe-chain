use crate::{
    constants::{TOKEN_NAME, TOKEN_SYMBOL},
    schema::NodContract,
};

impl NodContract<'_> {
    pub fn name() -> &'static str {
        TOKEN_NAME
    }

    pub fn symbol() -> &'static str {
        TOKEN_SYMBOL
    }
}
