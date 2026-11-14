#![cfg(feature = "test-protocol-overrides")]

use outbe_chain_constants::{
    get_metadosis_forming_period_seconds, initialize, ProtocolConstantsError,
};
use serde_json::json;

#[test]
fn invalid_override_does_not_initialize_the_singleton() {
    let forming = json!({ "metadosis": { "formingPeriodSeconds": 60 } });
    let forming_err = initialize(Some(&forming)).expect_err("forming is not an override");
    assert!(
        forming_err.to_string().contains("unknown field"),
        "{forming_err}"
    );
    assert!(
        forming_err.to_string().contains("formingPeriodSeconds"),
        "{forming_err}"
    );
    let offering = json!({ "metadosis": { "offeringPeriodSeconds": 120 } });
    let offering_err = initialize(Some(&offering)).expect_err("offering is not an override");
    assert!(
        offering_err.to_string().contains("unknown field"),
        "{offering_err}"
    );
    assert!(
        offering_err.to_string().contains("offeringPeriodSeconds"),
        "{offering_err}"
    );
    for lookback in [21 * 60 * 60, 23 * 60 * 60] {
        let invalid = json!({ "metadosis": { "lookbackDelaySeconds": lookback } });
        assert!(matches!(
            initialize(Some(&invalid)),
            Err(ProtocolConstantsError::InvalidValue {
                field: "metadosis.lookbackDelaySeconds",
                ..
            })
        ));
    }
    assert!(std::panic::catch_unwind(get_metadosis_forming_period_seconds).is_err());
}
