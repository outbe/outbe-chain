#![cfg(feature = "test-protocol-overrides")]

use outbe_chain_constants::{get_metadosis_forming_period_seconds, initialize};
use serde_json::json;

#[test]
fn invalid_override_does_not_initialize_the_singleton() {
    let lookback = json!({ "metadosis": { "lookbackDelaySeconds": 79_200 } });
    let lookback_err = initialize(Some(&lookback)).expect_err("lookback is not an override");
    assert!(
        lookback_err.to_string().contains("unknown field"),
        "{lookback_err}"
    );
    assert!(std::panic::catch_unwind(get_metadosis_forming_period_seconds).is_err());
}
