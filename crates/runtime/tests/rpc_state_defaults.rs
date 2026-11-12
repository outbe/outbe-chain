use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_rpc::state_reader::OutbeRpcStateReader;
use outbe_runtime::OutbeRuntime;
use outbe_slashindicator::constants::{
    DEFAULT_EVIDENCE_REWARD_PERCENT, DEFAULT_PROPOSER_FELONY_THRESHOLD,
    DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD, DEFAULT_SLASH_AMOUNT_PERCENT,
    DEFAULT_VOTER_FELONY_THRESHOLD, DEFAULT_VOTER_MISDEMEANOR_THRESHOLD,
};

#[test]
fn validator_reward_percent_matches_emission() {
    let info = OutbeRpcStateReader::emission_info(&OutbeRuntime::new());
    assert_eq!(
        info.validator_reward_percent,
        outbe_emission::allocation::VALIDATOR_REWARD_PCT
    );
}

#[test]
fn slash_config_reports_runtime_defaults_for_zero_storage() {
    let mut provider = HashMapStorageProvider::new(1);
    let config = StorageHandle::enter(&mut provider, |storage| {
        OutbeRuntime::new()
            .slash_config(storage)
            .expect("read slash config")
    });
    assert_eq!(
        config.proposer_misdemeanor_threshold,
        DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD
    );
    assert_eq!(
        config.proposer_felony_threshold,
        DEFAULT_PROPOSER_FELONY_THRESHOLD
    );
    assert_eq!(
        config.voter_misdemeanor_threshold,
        DEFAULT_VOTER_MISDEMEANOR_THRESHOLD
    );
    assert_eq!(
        config.voter_felony_threshold,
        DEFAULT_VOTER_FELONY_THRESHOLD
    );
    assert_eq!(config.slash_amount_percent, DEFAULT_SLASH_AMOUNT_PERCENT);
    assert_eq!(
        config.evidence_reward_percent,
        DEFAULT_EVIDENCE_REWARD_PERCENT
    );
}
