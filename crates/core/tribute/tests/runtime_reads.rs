use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::WorldwideDay,
    wwd_entity_id::derive_poseidon_entity_id,
};
use outbe_tribute::{TributeContract, TributeData};

fn tribute(owner: Address, day: u32) -> TributeData {
    let worldwide_day = WorldwideDay::from(day);
    TributeData {
        tribute_id: derive_poseidon_entity_id(owner, worldwide_day),
        owner,
        worldwide_day,
        issuance_amount_minor: U256::from(200),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(100),
        reference_currency: 840,
        tribute_price_minor: U256::from(2),
    }
}

fn in_process_ready<R>(
    storage: &StorageHandle<'_>,
    body: impl FnOnce() -> outbe_primitives::error::Result<R>,
) -> R {
    use outbe_primitives::storage::{metadosis_process_ready_binding, MetadosisCycleLifecycle};
    let binding = metadosis_process_ready_binding(
        storage.chain_id().unwrap(),
        storage.block_number().unwrap(),
        storage.now_secs().unwrap(),
    );
    storage
        .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, body)
        .unwrap()
}

fn encrypted_body() -> outbe_primitives::tribute_encryption::EncryptedTributeV2 {
    use outbe_primitives::tribute_encryption::{TributeAmountsV2, TributeContextV2};
    let plain = tribute(Address::repeat_byte(0x73), 20_260_723);
    outbe_tee_enclave::tribute_encryption::encrypt_tribute(
        &outbe_tribute::enclave_client::test_enclave::NETWORK_SECRET,
        TributeContextV2 {
            chain_id: 1,
            tribute_id: plain.tribute_id,
            owner: plain.owner,
            worldwide_day: plain.worldwide_day,
            issuance_currency: plain.issuance_currency,
            reference_currency: plain.reference_currency,
            tribute_price_minor: plain.tribute_price_minor,
            offer_input_hash: B256::repeat_byte(0x74),
        },
        &TributeAmountsV2 {
            issuance_amount_minor: plain.issuance_amount_minor,
            nominal_amount_minor: plain.nominal_amount_minor,
        },
    )
    .unwrap()
}

#[test]
fn encrypted_issue_preserves_ciphertext_events_and_public_reads_without_enclave() {
    use alloy_sol_types::{SolCall, SolEvent};
    use outbe_tribute::precompile::ITribute;
    let body = encrypted_body();
    let mut provider = HashMapStorageProvider::new(1);
    provider.enable_metadosis_mutation_frames(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
        2,
    );
    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage.clone());
        {
            let _enclave = outbe_tribute::enclave_client::test_enclave::scope();
            contract.issue_encrypted(&body).unwrap();
            assert_eq!(
                contract
                    .get_day_totals(body.context.worldwide_day)
                    .unwrap()
                    .tribute_nominal_total_minor,
                U256::from(100)
            );
        }
        let record = contract
            .get_record(body.context.tribute_id)
            .unwrap()
            .unwrap();
        assert_eq!(record.encrypted(), Some(&body));
        assert_eq!(
            contract.read_records_by_day(record.worldwide_day).unwrap(),
            vec![record.clone()]
        );
        assert_eq!(
            contract
                .load_tribute(record.tribute_id)
                .unwrap()
                .unwrap()
                .record(),
            &record
        );
        assert_eq!(contract.owner_of(record.tribute_id).unwrap(), record.owner);
        assert_eq!(
            contract
                .get_tribute_ids_by_day(record.worldwide_day)
                .unwrap(),
            vec![record.tribute_id]
        );
        assert_eq!(contract.balance_of(record.owner).unwrap(), 1);
        let uri = contract.token_uri(record.tribute_id).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(uri.strip_prefix("data:application/json;utf8,").unwrap()).unwrap();
        assert_eq!(json["attributes"][3]["value"]["word"], 0);
        assert_eq!(json["attributes"][4]["value"]["word"], 1);
        assert_eq!(
            json["attributes"][3]["value"]["ciphertext"],
            alloy_primitives::Bytes::from(body.encrypted_amounts.clone()).to_string()
        );
        assert!(record.calculation_view().is_err());
        assert!(contract.get_day_totals(record.worldwide_day).is_err());
        in_process_ready(&storage, || contract.seal_day(record.worldwide_day));
        assert!(contract.is_day_sealed(record.worldwide_day).unwrap());
        let call = ITribute::getDayTotalsCall {
            worldwideDay: record.worldwide_day.value(),
        };
        let output = outbe_tribute::precompile::dispatch(
            storage,
            &call.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
        .unwrap();
        let returned = ITribute::getDayTotalsCall::abi_decode_returns(&output).unwrap();
        assert_eq!(returned.tributeCount, 1);
        assert!(!returned.tributeNominalTotalMinor.is_empty());
    });
    let issued = provider
        .get_events(outbe_primitives::addresses::TRIBUTE_ADDRESS)
        .iter()
        .find_map(|event| ITribute::TributeIssued::decode_log_data(event).ok())
        .unwrap();
    assert_eq!(issued.issuanceAmountMinor.as_ref(), body.encrypted_amounts);
    assert_eq!(issued.nominalAmountMinor.as_ref(), body.encrypted_amounts);
}

#[test]
fn encrypted_issue_rejects_tampering_without_mutating_supply_or_day_amount() {
    let body = encrypted_body();
    let mut provider = HashMapStorageProvider::new(1);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = outbe_tribute::enclave_client::test_enclave::scope();
        let mut contract = TributeContract::new(storage.clone());
        let mut modified = body.clone();
        modified.encrypted_amounts[9] ^= 1;
        assert!(contract.issue_encrypted(&modified).is_err());
        assert_eq!(contract.total_supply().unwrap(), 0);
        assert_eq!(
            contract
                .get_day_totals(body.context.worldwide_day)
                .unwrap()
                .tribute_nominal_total_minor,
            U256::ZERO
        );
        assert!(contract
            .get_record(body.context.tribute_id)
            .unwrap()
            .is_none());
        contract.issue_encrypted(&body).unwrap();
        let live = contract
            .encrypted_day_nominal(body.context.worldwide_day, false)
            .unwrap();
        assert!(contract.issue_encrypted(&body).is_err());
        assert_eq!(
            contract
                .encrypted_day_nominal(body.context.worldwide_day, false)
                .unwrap(),
            live
        );
    });
}
