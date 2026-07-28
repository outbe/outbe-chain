// OCOMP-TEST-ID: OCM-VOT-001

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_metadosis::{
    ocomp::{
        schema::poc_schema_limits, test_support::ActivationFixture,
        vote::dispatch_public_result_vote,
    },
    precompile::IMetadosis,
    schema::MetadosisContract,
};
use outbe_ocomp_protocol::{state::OcompJobStatus, vote::OcompVoteAccountabilityV1};
use outbe_primitives::storage::StorageHandle;

fn result_digest(fixture: &ActivationFixture) -> B256 {
    fixture.result.result_digest(&fixture.limits).unwrap()
}

fn submit_vote(fixture: &mut ActivationFixture, validator_index: u8, height: u64) {
    fixture.provider.set_block_number(height);
    fixture.provider.enable_lysis_activation_frame();
    let vote = fixture.signed_result_vote(validator_index);
    let vote_bytes = vote.encode_canonical(&fixture.limits).unwrap();
    let calldata = IMetadosis::submitLysisResultCall {
        resultVoteV1: Bytes::from(vote_bytes),
    }
    .abi_encode();
    StorageHandle::enter(&mut fixture.provider, |storage| {
        assert_eq!(
            dispatch_public_result_vote(storage, &fixture.scope, &calldata, U256::ZERO, false,)
                .unwrap(),
            Bytes::new()
        );
    });
}

#[test]
fn ocm_vot_001_public_dispatch_records_four_slots_and_immutable_q3() {
    let mut fixture = ActivationFixture::new_voting(14, 1_010, true);
    let job_id = fixture.result.job_id;
    let intent_id = fixture.intent_id;
    let expected_result_digest = result_digest(&fixture);

    for index in 0..4 {
        submit_vote(&mut fixture, index, 14 + u64::from(index));
    }

    StorageHandle::enter(&mut fixture.provider, |storage| {
        let limits = poc_schema_limits();
        let contract = MetadosisContract::new(storage.clone());
        let record = contract
            .ocomp_job_record(intent_id, &limits)
            .unwrap()
            .unwrap();
        assert_eq!(record.status, OcompJobStatus::Completed);
        let quorum = record.finalized.unwrap().quorum.unwrap();
        assert_eq!(quorum.result_digest, expected_result_digest);
        assert_eq!(quorum.signer_bitmap, 0b0111);

        let encoded = outbe_metadosis::precompile::dispatch(
            storage,
            &IMetadosis::getOffchainVoteAccountabilityCall { jobId: job_id }.abi_encode(),
            Address::repeat_byte(0x41),
            U256::ZERO,
        )
        .unwrap();
        let public =
            IMetadosis::getOffchainVoteAccountabilityCall::abi_decode_returns(&encoded).unwrap();
        let accountability =
            OcompVoteAccountabilityV1::decode_canonical(public.as_ref(), &limits).unwrap();
        assert_eq!(accountability.slots.iter().flatten().count(), 4);
        assert_eq!(accountability.quorum.unwrap(), quorum);
    });
}
