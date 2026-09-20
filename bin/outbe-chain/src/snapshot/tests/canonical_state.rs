use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_ocomp_protocol::state::OcompJobRecordV1;

use super::super::{
    native::RethReadOnlyView,
    validation::{canonical_state::CanonicalState, evm::verify_current_evm, headers::HeaderAudit},
};

#[test]
fn scratch_storage_lookup_requires_the_exact_hashed_slot() {
    for version in [1, 2] {
        let (_source, layout, _) = super::evm::state_fixture(version);
        let scratch = tempfile::tempdir().unwrap();
        let source = RethReadOnlyView::open(&layout).unwrap();
        let verified = verify_current_evm(&source, scratch.path()).unwrap();
        // Storage-only checks need no historical header identities.
        let headers = HeaderAudit {
            intervals: Vec::new(),
            required_missing: Vec::new(),
            verified_headers: 0,
        };
        let state = CanonicalState::new(&verified, &source, &headers);
        let address = Address::repeat_byte(0x11);
        let present = B256::repeat_byte(0x22);
        let absent = (0_u64..1024)
            .map(|number| B256::from(U256::from(number).to_be_bytes::<32>()))
            .find(|slot| keccak256(slot) < keccak256(present))
            .expect("a missing slot preceding the native duplicate");

        assert_eq!(
            state
                .with_storage(|storage| storage.sload(address, U256::from_be_bytes(present.0)))
                .unwrap(),
            U256::from(123)
        );
        assert_eq!(
            state
                .with_storage(|storage| storage.sload(address, U256::from_be_bytes(absent.0)))
                .unwrap(),
            U256::ZERO,
            "seek_by_key_subkey may return the adjacent greater slot"
        );
    }
}

#[test]
fn external_live_job_view_exposes_public_protocol_types() {
    let (_source, layout, _) = super::evm::state_fixture(2);
    let scratch = tempfile::tempdir().unwrap();
    let source = RethReadOnlyView::open(&layout).unwrap();
    let verified = verify_current_evm(&source, scratch.path()).unwrap();
    let headers = HeaderAudit {
        intervals: Vec::new(),
        required_missing: Vec::new(),
        verified_headers: 0,
    };
    let state = CanonicalState::new(&verified, &source, &headers);
    let owner_jobs: Vec<(B256, OcompJobRecordV1)> = state
        .with_storage(outbe_metadosis::api::read_live_ocomp_jobs)
        .unwrap();
    let adapter_jobs: Vec<(B256, OcompJobRecordV1)> = state.live_ocomp_jobs().unwrap();
    assert!(owner_jobs.is_empty());
    assert_eq!(adapter_jobs, owner_jobs);
}

