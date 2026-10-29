#![cfg(feature = "test-protocol-overrides")]

use outbe_chain_constants::{
    get_metadosis_advance_interval_seconds, get_metadosis_forming_period_seconds,
    get_metadosis_lookback_delay_seconds, get_metadosis_offering_period_seconds, initialize,
    ProtocolConstantsError, DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS,
    DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS,
};
use serde_json::json;

#[test]
fn test_initialization_resolves_genesis_overrides() {
    let lookback = 22 * 3_600;
    let overrides = json!({
        "governance": { "votingWindowBlocks": 17 },
        "metadosis": {
            "lookbackDelaySeconds": lookback,
            "advanceIntervalSeconds": 3_600
        },
    });

    initialize(Some(&overrides)).unwrap();
    assert_eq!(
        outbe_chain_constants::get_governance_voting_window_blocks(),
        17
    );

    assert_eq!(
        get_metadosis_forming_period_seconds(),
        DEFAULT_METADOSIS_FORMING_PERIOD_SECONDS
    );
    assert_eq!(get_metadosis_lookback_delay_seconds(), lookback);
    assert_eq!(
        get_metadosis_offering_period_seconds(),
        DEFAULT_METADOSIS_OFFERING_PERIOD_SECONDS
    );
    assert_eq!(get_metadosis_advance_interval_seconds(), 3_600);
    assert_eq!(
        initialize(None),
        Err(ProtocolConstantsError::AlreadyInitialized)
    );
}
