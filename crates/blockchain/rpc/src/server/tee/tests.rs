use super::*;
use outbe_primitives::{
    chain::TESTNET_CHAIN_ID,
    storage::hashmap::HashMapStorageProvider,
    tee_attestation_v1::{
        AttestationMode, PlatformTcbStatusSetV1, QvlTcbStatusV1, TeeMeasurementRuleV1, TeePolicyV1,
    },
};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
const GENESIS: B256 = B256::repeat_byte(0x21);
const MRENCLAVE: B256 = B256::repeat_byte(0x81);
const MRSIGNER: B256 = B256::repeat_byte(0x82);
fn policy() -> TeePolicyV1 {
    TeePolicyV1 {
        policy_version: 1,
        chain_id: U256::from(TESTNET_CHAIN_ID).to_be_bytes(),
        genesis_hash: GENESIS,
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash: B256::repeat_byte(0x71),
        quote_version: 3,
        tee_type: 0,
        attestation_key_type: 2,
        qe_vendor_id: [
            0x93, 0x9a, 0x72, 0x33, 0xf7, 0x9c, 0x4c, 0xa9, 0x94, 0x0a, 0x0d, 0xb3, 0x95, 0x7f,
            0x06, 0x07,
        ],
        certification_data_type: 5,
        tcb_info_schema_version: 3,
        qe_identity_schema_version: 2,
        minimum_tcb_evaluation_data_number: 1,
        accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOnly,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: 604_800,
        collateral_margin: 3_600,
        resource_schedule_hash: B256::repeat_byte(0x72),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            mrenclave: MRENCLAVE,
            mrsigner: MRSIGNER,
            isv_prod_id: 7,
            minimum_isv_svn: 3,
            admit_from_height: 1,
            admit_until_height_exclusive: 100,
        }],
    }
}

fn context() -> DcapOnboardingContextV1 {
    DcapOnboardingContextV1 {
        chain_id: U256::from(TESTNET_CHAIN_ID).to_be_bytes(),
        genesis_hash: GENESIS,
        intent_hash: B256::repeat_byte(1),
        node_id_hash: B256::repeat_byte(2),
        enclave_id: B256::repeat_byte(3),
        binding_id: B256::repeat_byte(4),
        policy_hash: B256::repeat_byte(5),
        recipient_x25519: [6; 32],
        tribute_offer_public: [7; 32],
        key_epoch: 8,
        tribute_offer_epoch: 9,
    }
}
fn candidate_storage(context: &DcapOnboardingContextV1) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new_with_chain_identity(TESTNET_CHAIN_ID, GENESIS);
    provider.set_block_number(10);
    provider.set_timestamp(U256::from(100));
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage);
        registry.install_initial_policy_v1(&policy()).unwrap();
        registry
            .upgrade_candidate_context
            .write(&context.node_id_hash, context.context_hash())
            .unwrap();
        registry
            .upgrade_candidate_expiry
            .write(&context.node_id_hash, 101)
            .unwrap();
        registry
            .upgrade_candidate_source
            .write(&context.node_id_hash, context.binding_id)
            .unwrap();
        registry
            .v1_node_binding_id
            .write(&context.node_id_hash, context.binding_id)
            .unwrap();
        registry
            .strict_upgrade_successor
            .write(context.policy_hash)
            .unwrap();
        registry
            .tribute_offer_public_key
            .write(B256::from(context.tribute_offer_public))
            .unwrap();
        registry.key_epoch.write(context.key_epoch).unwrap();
        registry
            .tribute_offer_epoch
            .write(context.tribute_offer_epoch)
            .unwrap();
    });
    provider
}
struct RecordingReader<'a> {
    words: &'a HashMap<(Address, U256), U256>,
    reads: Rc<RefCell<Vec<B256>>>,
    fail_at: Option<usize>,
}
impl StorageReader for RecordingReader<'_> {
    fn read_storage(&self, address: Address, key: B256) -> outbe_primitives::error::Result<U256> {
        let mut reads = self.reads.borrow_mut();
        reads.push(key);
        if self.fail_at == Some(reads.len()) {
            return Err(outbe_primitives::error::PrecompileError::Storage(
                "injected read failure".into(),
            ));
        }
        Ok(self
            .words
            .get(&(address, U256::from_be_bytes(key.0)))
            .copied()
            .unwrap_or_default())
    }
}
fn check(
    provider: &HashMapStorageProvider,
    context: &DcapOnboardingContextV1,
    legacy: bool,
    fail_at: Option<usize>,
) -> (Result<bool, String>, Vec<B256>) {
    let reads = Rc::new(RefCell::new(Vec::new()));
    let reader = RecordingReader {
        words: &provider.storage,
        reads: Rc::clone(&reads),
        fail_at,
    };
    let mut provider = ReadOnlyStorageProvider::new_with_block_context(
        reader,
        ReadOnlyBlockContext {
            chain_id: TESTNET_CHAIN_ID,
            genesis_hash: GENESIS,
            block_number: 10,
            timestamp: 100,
        },
    );
    let result = live_upgrade_candidate(
        &TeeRegistry::new(StorageHandle::new(&mut provider)),
        context,
        100,
        legacy,
    )
    .map_err(|e| e.to_string());
    let recorded = reads.borrow().clone();
    (result, recorded)
}
#[test]
fn upgrade_candidate_preserves_order_repeated_binding_read_and_short_circuit() {
    let context = context();
    let mut provider = candidate_storage(&context);
    let (result, reads) = check(&provider, &context, false, None);
    assert_eq!(result, Ok(true));
    // Independent trace from the old predicate: context, expiry, source, binding,
    // binding again, successor, offer key, key epoch, offer epoch.
    let tail = &reads[reads.len() - 9..];
    assert_eq!(tail[3], tail[4]);
    for fail_at in 1..=reads.len() {
        let (result, failed) = check(&provider, &context, false, Some(fail_at));
        assert!(result.unwrap_err().contains("injected read failure"));
        assert_eq!(failed, &reads[..fail_at]);
    }
    StorageHandle::enter(&mut provider, |storage| {
        TeeRegistry::new(storage)
            .upgrade_candidate_context
            .write(&context.node_id_hash, B256::ZERO)
    })
    .unwrap();
    let (result, rejected) = check(&provider, &context, false, None);
    assert_eq!(result, Ok(false));
    assert_eq!(rejected, &reads[..reads.len() - 8]);
    // Missing policy rejects before even the mismatched candidate context read.
    let missing = HashMapStorageProvider::new_with_chain_identity(TESTNET_CHAIN_ID, GENESIS);
    let (result, reads) = check(&missing, &context, false, None);
    assert!(result.unwrap_err().contains("policy is not installed"));
    assert_eq!(reads.len(), 1);
}
#[test]
fn upgrade_candidate_expiry_and_attestation_rejection_preserve_precedence() {
    let context = context();
    let mut provider = candidate_storage(&context);
    let (_, reads) = check(&provider, &context, false, None);
    assert_eq!(check(&provider, &context, true, None).0, Ok(false));
    StorageHandle::enter(&mut provider, |storage| {
        TeeRegistry::new(storage)
            .upgrade_candidate_expiry
            .write(&context.node_id_hash, 100)
    })
    .unwrap();
    let (result, rejected) = check(&provider, &context, false, None);
    assert_eq!(result, Ok(false));
    assert_eq!(rejected, &reads[..reads.len() - 7]);
}
fn renewal_epoch(number: U256, start: u64, length: u32) -> RenewalEpoch {
    RenewalEpoch {
        finalized_height: 10,
        finalized_hash: B256::repeat_byte(1),
        finalized_timestamp: 100,
        epoch: outbe_validatorset::EpochSnapshot {
            number,
            start_timestamp: 0,
            start_block: start,
            length_blocks: length,
        },
    }
}
#[test]
fn renewal_schedule_caps_window_and_keeps_checked_arithmetic_error_order() {
    let config = TeeRenewalScheduleConfigV1 {
        dkg_prepare_window_blocks: 100,
        minimum_block_time_millis: 1000,
    };
    let schedule = renewal_epoch(U256::from(1), 5, 20)
        .schedule(config)
        .unwrap();
    assert_eq!(
        (
            schedule.next_freeze_height,
            schedule.planned_activation_height
        ),
        (5, 25)
    );
    assert_eq!(schedule.dkg_prepare_window_blocks, 20);
    for (number, start, length, message) in [
        (
            U256::from(u64::MAX) + U256::from(1),
            u64::MAX,
            0,
            "finalized epoch number exceeds u64",
        ),
        (U256::ZERO, u64::MAX, 0, "finalized epoch length is zero"),
        (
            U256::ZERO,
            u64::MAX,
            1,
            "planned activation height overflow",
        ),
    ] {
        let error = renewal_epoch(number, start, length)
            .schedule(config)
            .unwrap_err();
        assert_eq!(error.message(), message);
        assert_eq!(error.code(), jsonrpsee::types::error::INTERNAL_ERROR_CODE);
    }
}
