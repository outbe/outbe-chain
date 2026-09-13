use super::*;

fn materialization_head(
    last_progress_height: u64,
) -> outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1 {
    outbe_ocomp_protocol::nod_materialization::NodMaterializationHeadV1 {
        queue_sequence: 1,
        job_id: B256::repeat_byte(0x11),
        program_semantics_hash: B256::repeat_byte(0x22),
        worldwide_day: 20_260_812,
        generation: 1,
        nod_root: B256::repeat_byte(0x33),
        nod_count: 10,
        next_nod_ordinal: 8,
        last_progress_height,
    }
}

#[test]
fn finalized_materialization_wake_uses_the_authenticated_system_tx_signer() {
    use outbe_primitives::{
        signer::OutbeEvmSigner,
        system_tx::{build_unsigned_system_tx, SystemTxInputV2},
    };

    let signer = OutbeEvmSigner::from_secret_bytes([0x41; 32]).expect("test signer");
    let input = SystemTxInputV2::CycleTick;
    let unsigned = build_unsigned_system_tx(
        input.kind(),
        0,
        2,
        outbe_primitives::chain::CHAIN_ID,
        input.encode().expect("canonical system input"),
    )
    .expect("unsigned system transaction");
    let signed = signer
        .sign_unsigned(unsigned)
        .expect("signed system transaction");

    let transactions = [signed];
    let proposer = authenticated_finalized_proposer(transactions.iter())
        .expect("recover authenticated finalized proposer");
    assert_eq!(proposer, signer.address());
    assert_ne!(
        proposer,
        outbe_primitives::addresses::REWARDS_ADDRESS,
        "the protocol reward beneficiary is not proposer identity"
    );
    assert!(should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(signer.address()),
        proposer,
        100,
        Some(&materialization_head(90)),
        true,
        30,
    ));
}

#[test]
fn genesis_without_system_transactions_has_no_materialization_proposer() {
    let transactions: Vec<outbe_primitives::OutbeTxEnvelope> = Vec::new();
    assert_eq!(
        finalized_materialization_proposer(0, transactions.iter())
            .expect("genesis has no materialization proposer"),
        None
    );
}

#[test]
fn only_the_validator_representing_the_finalized_proposer_wakes_materialization() {
    let represented = alloy_primitives::Address::repeat_byte(0x11);
    let other = alloy_primitives::Address::repeat_byte(0x22);
    let head = materialization_head(90);

    assert!(should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(represented),
        represented,
        100,
        Some(&head),
        true,
        30,
    ));
    assert!(!should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(represented),
        other,
        100,
        Some(&head),
        true,
        30,
    ));
    assert!(!should_wake_nod_materializer(
        EmbeddedNodePolicyV1::FullNode,
        None,
        represented,
        100,
        Some(&head),
        true,
        30,
    ));
}

#[test]
fn retry_wake_uses_the_resolved_interval_and_requires_an_incomplete_head() {
    let represented = alloy_primitives::Address::repeat_byte(0x11);
    let head = materialization_head(90);

    assert!(!should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(represented),
        represented,
        119,
        Some(&head),
        false,
        30,
    ));
    assert!(should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(represented),
        represented,
        120,
        Some(&head),
        false,
        30,
    ));
    assert!(!should_wake_nod_materializer(
        EmbeddedNodePolicyV1::Validator,
        Some(represented),
        represented,
        120,
        None,
        true,
        30,
    ));
}

#[test]
fn materialization_retry_memory_tracks_only_the_current_head() {
    let old = MaterializationAttemptKeyV1 {
        queue_sequence: 1,
        first_nod_ordinal: 0,
    };
    let current = MaterializationAttemptKeyV1 {
        queue_sequence: 2,
        first_nod_ordinal: 8,
    };
    let mut attempts = BTreeMap::from([(old, 10), (current, 20)]);
    bound_materialization_attempts(&mut attempts, Some(current));
    assert_eq!(attempts, BTreeMap::from([(current, 20)]));
    bound_materialization_attempts(&mut attempts, None);
    assert!(attempts.is_empty());
}

#[test]
fn pre_finalization_request_blocks_checkpoint_until_job_materializes() {
    let intent_id = B256::repeat_byte(0x41);
    let mut materialized = std::collections::BTreeSet::new();
    assert!(!all_requests_materialized(
        [intent_id].into_iter(),
        &materialized
    ));
    materialized.insert(intent_id);
    assert!(all_requests_materialized(
        [intent_id].into_iter(),
        &materialized
    ));
}

#[cfg(test)]
fn all_requests_materialized(
    request_ids: impl IntoIterator<Item = B256>,
    materialized_requests: &BTreeSet<B256>,
) -> bool {
    request_ids
        .into_iter()
        .all(|intent_id| materialized_requests.contains(&intent_id))
}
