//! Frozen domain enum layout with populated nested fields.
use std::sync::{Arc, Mutex};

use super::enclave_response::EnclaveResponse;
use alloy_primitives::{hex, B256, U256};
use outbe_core_common::confidential_tee::{
    consensus_witness_call, WitnessDomainWire, WitnessSchema,
};
use outbe_primitives::tee_attestation_v1::{AttestationMode, NetworkBindingV1};
use outbe_tee::witness::{
    EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessExecutionScope,
    WitnessState,
};
use outbe_tee::{CallMeta, TransportError};
use outbe_tribute::confidential::host::{
    EncryptedTributeOfferBatchV2, LysisFractionsExecutedV1, NodEncryptedForTributeV1,
    NodIssuedFromTributeV1, TributeAmountsReadV2, TributeDayAmountReadV2,
    TributeDayFractionsReadV1, TributeDayOpAppliedV2,
};

fn vectors() -> &'static [(usize, &'static str, &'static str)] {
    &[
// 0: TributeOfferBatch
(0, r#"{"TributeOfferBatch":{"results":[{"token_id":"0x6565656565656565656565656565656565656565656565656565656565656565","owner":"0x2222222222222222222222222222222222222222","issuance_amount_minor":"0x11","nominal_amount_minor":"0x12","effective_reference_price_minor":"0x13","su_hashes":["su-a"],"wallet_addresses":["wallet-b"],"sra_addresses":["sra-c"],"zk_expected_hashes":{"nft_hash":"0x6161616161616161616161616161616161616161616161616161616161616161","binding_hash":"0x6262626262626262626262626262626262626262626262626262626262626262"},"status":"Created"}],"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "0001206565656565656565656565656565656565656565656565656565656565656565142222222222222222222222222222222222222222200000000000000000000000000000000000000000000000000000000000000011200000000000000000000000000000000000000000000000000000000000000012200000000000000000000000000000000000000000000000000000000000000013010473752d61010877616c6c65742d6201057372612d630120616161616161616161616161616161616161616161616161616161616161616120626262626262626262626262626262626262626262626262626262626262626200201111111111111111111111111111111111111111111111111111111111111111025152"),
// 1: GratisOpApplied
(1, r#"{"GratisOpApplied":{"result":{"status":"Applied","new_balance":[33],"new_pledged":[34],"event_amount":"0x11","next_op_nonce":5,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}}}"#, "01000121012220000000000000000000000000000000000000000000000000000000000000001105010121017b072011111111111111111111111111111111111111111111111111111111111111110251520123"),
// 2: PromisOpApplied
(2, r#"{"PromisOpApplied":{"result":{"status":"Applied","new_balance":[33],"event_amount":"0x11","next_op_nonce":5,"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}}}"#, "02000121200000000000000000000000000000000000000000000000000000000000000011052011111111111111111111111111111111111111111111111111111111111111110251520123"),
// 3: FidelityCohortApplied
(3, r#"{"FidelityCohortApplied":{"result":{"outcome":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}}"#, "030121017b07201111111111111111111111111111111111111111111111111111111111111111025152"),
// 4: FidelityLeaguesSnapshotted
(4, r#"{"FidelityLeaguesSnapshotted":{"leagues":[{"owner":"0x2222222222222222222222222222222222222222","league":7,"rcfi":"0x11"}],"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "040114222222222222222222222222222222222222222207200000000000000000000000000000000000000000000000000000000000000011201111111111111111111111111111111111111111111111111111111111111111025152"),
// 5: FidelityIndexQueried
(5, r#"{"FidelityIndexQueried":{"result":{"rcfi":"0x11","efficiency":"0x12","league":7,"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}}"#, "0520000000000000000000000000000000000000000000000000000000000000001120000000000000000000000000000000000000000000000000000000000000001207201111111111111111111111111111111111111111111111111111111111111111025152"),
// 6: AccountKeysSealed
(6, r#"{"AccountKeysSealed":{"account":"0x2222222222222222222222222222222222222222","sealed":[65,66],"nonce":[49,49,49,49,49,49,49,49,49,49,49,49],"enclave_ephemeral_pubkey":[50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50,50]}}"#, "061422222222222222222222222222222222222222220241423131313131313131313131313232323232323232323232323232323232323232323232323232323232323232"),
// 7: NodEncryptedForTributeV1
(7, r#"{"NodEncryptedForTributeV1":{"nod":{"terms":{"chain_id":1,"nod_id":"0x2","owner":"0x2222222222222222222222222222222222222222","emission_day":20261116,"league_id":7,"entry_price_minor":"0x3","issuance_currency":840,"reference_currency":978},"encryption_binding":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_gratis_amount":[65]},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "0701200000000000000000000000000000000000000000000000000000000000000002142222222222222222222222222222222222222222fcd1d40907200000000000000000000000000000000000000000000000000000000000000003c806d2072033333333333333333333333333333333333333333333333333333333333333330141201111111111111111111111111111111111111111111111111111111111111111025152"),
// 8: EncryptedNodMintRejectedV1
(8, r#"{"EncryptedNodMintRejectedV1":{"reason":"rejected","inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "080872656a6563746564201111111111111111111111111111111111111111111111111111111111111111025152"),
// 9: EncryptedNodMinedV1
(9, r#"{"EncryptedNodMinedV1":{"result":{"new_balance":[33],"next_op_nonce":5,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}}}"#, "090121050121017b072011111111111111111111111111111111111111111111111111111111111111110251520123"),
// 10: NodAmountReadV1
(10, r#"{"NodAmountReadV1":{"amount":"0x11","inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "0a200000000000000000000000000000000000000000000000000000000000000011201111111111111111111111111111111111111111111111111111111111111111025152"),
// 11: EncryptedTributeOfferBatchV2
(11, r#"{"EncryptedTributeOfferBatchV2":{"results":[{"token_id":"0x6565656565656565656565656565656565656565656565656565656565656565","tribute":{"context":{"chain_id":1,"tribute_id":"0x013528fc63636363636363636363636363636363636363636363636363636363","owner":"0x2222222222222222222222222222222222222222","worldwide_day":20261116,"issuance_currency":840,"reference_currency":978,"tribute_price_minor":"0x13","offer_input_hash":"0x6464646464646464646464646464646464646464646464646464646464646464"},"encrypted_amounts":[65,66]},"su_hashes":["su-a"],"wallet_addresses":["wallet-b"],"sra_addresses":["sra-c"],"zk_expected_hashes":{"nft_hash":"0x6161616161616161616161616161616161616161616161616161616161616161","binding_hash":"0x6262626262626262626262626262626262626262626262626262626262626262"},"status":"Created"}],"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "0b01206565656565656565656565656565656565656565656565656565656565656565010120013528fc63636363636363636363636363636363636363636363636363636363142222222222222222222222222222222222222222fcd1d409c806d207200000000000000000000000000000000000000000000000000000000000000013206464646464646464646464646464646464646464646464646464646464646464024142010473752d61010877616c6c65742d6201057372612d630120616161616161616161616161616161616161616161616161616161616161616120626262626262626262626262626262626262626262626262626262626262626200201111111111111111111111111111111111111111111111111111111111111111025152"),
// 12: TributeAmountsReadV2
(12, r#"{"TributeAmountsReadV2":{"amounts":[{"issuance_amount_minor":"0x11","nominal_amount_minor":"0x12"}],"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "0c01200000000000000000000000000000000000000000000000000000000000000011200000000000000000000000000000000000000000000000000000000000000012201111111111111111111111111111111111111111111111111111111111111111025152"),
// 13: TributeDayOpAppliedV2
(13, r#"{"TributeDayOpAppliedV2":{"record":{"chain_id":1,"worldwide_day":20261116,"frozen":true,"operation_hash":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_amount":[65]},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "0d01fcd1d409012033333333333333333333333333333333333333333333333333333333333333330141201111111111111111111111111111111111111111111111111111111111111111025152"),
// 14: TributeDayAmountReadV2
(14, r#"{"TributeDayAmountReadV2":{"amount":"0x11","inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "0e200000000000000000000000000000000000000000000000000000000000000011201111111111111111111111111111111111111111111111111111111111111111025152"),
// 15: FidelityQueryRejected
(15, r#"{"FidelityQueryRejected":{"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","reason":"rejected","attestation_tag":[81,82]}}"#, "0f2011111111111111111111111111111111111111111111111111111111111111110872656a6563746564025152"),
// 16: LysisFractionsExecutedV1
(16, r#"{"LysisFractionsExecutedV1":{"result":{"day_fractions":{"chain_id":1,"worldwide_day":20261116,"emission_day":20261115,"completed_at":123,"operation_hash":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_fractions":[65]},"total_nominal_minor":"0x11","gratis_demand_minor":"0x12","lysis_limit_minor":"0x13","allocated_minor":"0x14","zero_load_tribute_ids":["0x013528fc63636363636363636363636363636363636363636363636363636363"],"series_loads":[{"scope":"0x6666666666666666666666666666666666666666666666666666666666666666","unclaimed_tribute_count":3,"unclaimed_tribute_load":{"chain_id":1,"scope":"0x6767676767676767676767676767676767676767676767676767676767676767","operation_hash":"0x6868686868686868686868686868686868686868686868686868686868686868","encrypted_load":[67,68]}}]},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "1001fcd1d409fbd1d4097b20333333333333333333333333333333333333333333333333333333333333333301412000000000000000000000000000000000000000000000000000000000000000112000000000000000000000000000000000000000000000000000000000000000122000000000000000000000000000000000000000000000000000000000000000132000000000000000000000000000000000000000000000000000000000000000140120013528fc63636363636363636363636363636363636363636363636363636363012066666666666666666666666666666666666666666666666666666666666666660301206767676767676767676767676767676767676767676767676767676767676767206868686868686868686868686868686868686868686868686868686868686868024344201111111111111111111111111111111111111111111111111111111111111111025152"),
// 17: TributeDayFractionsReadV1
(17, r#"{"TributeDayFractionsReadV1":{"fractions":{"3":17,"7":19},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "110203110713201111111111111111111111111111111111111111111111111111111111111111025152"),
// 18: NodIssuedFromTributeV1
(18, r#"{"NodIssuedFromTributeV1":{"outcome":"ZeroSettlementCost","inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "1202201111111111111111111111111111111111111111111111111111111111111111025152"),
// 19: NodGroupLoadOpAppliedV1
(19, r#"{"NodGroupLoadOpAppliedV1":{"record":{"chain_id":1,"scope":"0x2222222222222222222222222222222222222222222222222222222222222222","operation_hash":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_load":[65]},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "13012022222222222222222222222222222222222222222222222222222222222222222033333333333333333333333333333333333333333333333333333333333333330141201111111111111111111111111111111111111111111111111111111111111111025152"),
// 20: NodGroupLoadReadV1
(20, r#"{"NodGroupLoadReadV1":{"amount":"0x11","inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "14200000000000000000000000000000000000000000000000000000000000000011201111111111111111111111111111111111111111111111111111111111111111025152"),
// 21: NodQuoteEncryptedV1
(21, r#"{"NodQuoteEncryptedV1":{"encrypted_payment":[65,66],"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "15024142201111111111111111111111111111111111111111111111111111111111111111025152"),
// 22: GratisMinedFromPromisV1
(22, r#"{"GratisMinedFromPromisV1":{"result":{"status":"Applied","new_promis_balance":[33],"encrypted_promis_event_amount":[34],"next_promis_op_nonce":5,"new_gratis_balance":[35],"encrypted_gratis_event_amount":[36],"next_gratis_op_nonce":6,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}}"#, "160001210122050123012406010121017b07201111111111111111111111111111111111111111111111111111111111111111025152"),
// 1: GratisOpApplied
(1, r#"{"GratisOpApplied":{"result":{"status":{"Rejected":{"reason":"gratis rejected"}},"new_balance":[33],"new_pledged":[34],"event_amount":"0x11","next_op_nonce":5,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}}}"#, "01010f6772617469732072656a65637465640121012220000000000000000000000000000000000000000000000000000000000000001105010121017b072011111111111111111111111111111111111111111111111111111111111111110251520123"),
// 2: PromisOpApplied
(2, r#"{"PromisOpApplied":{"result":{"status":{"Rejected":{"reason":"promis rejected"}},"new_balance":[33],"event_amount":"0x11","next_op_nonce":5,"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82],"encrypted_event_amount":[35]}}}"#, "02010f70726f6d69732072656a65637465640121200000000000000000000000000000000000000000000000000000000000000011052011111111111111111111111111111111111111111111111111111111111111110251520123"),
// 22: GratisMinedFromPromisV1
(22, r#"{"GratisMinedFromPromisV1":{"result":{"status":{"PromisRejected":{"reason":"promis rejected"}},"new_promis_balance":[33],"encrypted_promis_event_amount":[34],"next_promis_op_nonce":5,"new_gratis_balance":[35],"encrypted_gratis_event_amount":[36],"next_gratis_op_nonce":6,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}}"#, "16010f70726f6d69732072656a656374656401210122050123012406010121017b07201111111111111111111111111111111111111111111111111111111111111111025152"),
// 22: GratisMinedFromPromisV1
(22, r#"{"GratisMinedFromPromisV1":{"result":{"status":{"GratisRejected":{"reason":"gratis rejected"}},"new_promis_balance":[33],"encrypted_promis_event_amount":[34],"next_promis_op_nonce":5,"new_gratis_balance":[35],"encrypted_gratis_event_amount":[36],"next_gratis_op_nonce":6,"fidelity":{"new_blob":[33],"qualified_start_initialized":123,"league":7},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}}"#, "16020f6772617469732072656a656374656401210122050123012406010121017b07201111111111111111111111111111111111111111111111111111111111111111025152"),
// 18: NodIssuedFromTributeV1
(18, r#"{"NodIssuedFromTributeV1":{"outcome":{"Issued":{"terms":{"chain_id":1,"nod_id":"0x2","owner":"0x2222222222222222222222222222222222222222","emission_day":20261116,"league_id":7,"entry_price_minor":"0x3","issuance_currency":840,"reference_currency":978},"encryption_binding":"0x3333333333333333333333333333333333333333333333333333333333333333","encrypted_gratis_amount":[65]}},"inputs_canonical_hash":"0x1111111111111111111111111111111111111111111111111111111111111111","attestation_tag":[81,82]}}"#, "120001200000000000000000000000000000000000000000000000000000000000000002142222222222222222222222222222222222222222fcd1d40907200000000000000000000000000000000000000000000000000000000000000003c806d2072033333333333333333333333333333333333333333333333333333333333333330141201111111111111111111111111111111111111111111111111111111111111111025152"),
    ]
}

#[test]
fn domain_response_postcard_vectors() {
    let mut seen = [false; 23];
    for &(index, json, encoded) in vectors() {
        seen[index] = true;
        let expected: EnclaveResponse = serde_json::from_str(json).unwrap();
        let bytes = hex::decode(encoded).unwrap();
        assert_eq!(usize::from(bytes[0]), index);
        assert_eq!(
            postcard::to_allocvec(&expected).unwrap(),
            bytes,
            "variant {index}"
        );
        let decoded: EnclaveResponse = postcard::from_bytes(&bytes).unwrap();
        assert!(decoded == expected, "variant {index}");
    }
    assert!(seen.into_iter().all(|present| present));
}

fn tag_free_payload(encoded: &str) -> Vec<u8> {
    let bytes = hex::decode(encoded).unwrap();
    let at = bytes
        .windows(3)
        .position(|window| window == [2, 0x51, 0x52])
        .expect("attestation vec");
    assert_eq!(
        bytes
            .windows(3)
            .filter(|window| *window == [2, 0x51, 0x52])
            .count(),
        1
    );
    let mut payload = Vec::with_capacity(bytes.len() - 2);
    payload.extend_from_slice(&bytes[..at]);
    payload.push(0);
    payload.extend_from_slice(&bytes[at + 3..]);
    payload
}

fn accept_frozen_tag<T: WitnessDomainWire>(
    _key: &[u8; 32],
    value: &T,
) -> Result<(), TransportError> {
    if value.attestation_tag() == [81, 82] {
        Ok(())
    } else {
        Err(TransportError::UnexpectedResponse)
    }
}

fn pin_production_wire<T>(json: &str, encoded: &str)
where
    T: WitnessSchema + PartialEq + std::fmt::Debug,
{
    let expected = tag_free_payload(encoded);
    let outer: serde_json::Map<String, serde_json::Value> = serde_json::from_str(json).unwrap();
    assert_eq!(outer.len(), 1);
    let value: T = serde_json::from_value(outer.into_values().next().unwrap()).unwrap();
    assert_eq!(T::TAG.value(), expected[0]);
    assert_eq!(WitnessDomainWire::attestation_tag(&value), &[81, 82]);

    let context = Arc::new(outbe_tee::test_utils::resident_context(
        4,
        NetworkBindingV1 {
            chain_id: U256::from(1u64).to_be_bytes(),
            genesis_hash: B256::repeat_byte(2),
            attestation_mode: AttestationMode::NoAttestationDev,
        },
    ));
    let leader = context.attestation_public();
    let _guard = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some((
        context,
        |_context, _lane, _bytes| Err(TransportError::UnexpectedResponse),
    )));
    let state = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::ProposerRecord,
        Some(leader),
        EnclaveWitnessTapeV1::default(),
    )));
    let scope = WitnessExecutionScope::enter(state.clone());
    assert_eq!(
        consensus_witness_call(
            CallMeta::recorded("tribute-schema-pin"),
            {
                let value = value.clone();
                move |_| Ok(value)
            },
            accept_frozen_tag,
        )
        .unwrap(),
        value
    );
    let staged = state.lock().unwrap().staged.clone();
    drop(scope);
    let EnclaveWitnessEntryV1::Payload {
        payload,
        inputs_canonical_hash,
        attestation_tag,
    } = &staged[0]
    else {
        panic!("recorded entry is not Payload");
    };
    assert_eq!(payload, &expected);
    assert_eq!(
        *inputs_canonical_hash,
        WitnessDomainWire::inputs_canonical_hash(&value)
    );
    assert_eq!(attestation_tag.as_slice(), [81, 82]);

    let replay = Arc::new(Mutex::new(WitnessState::new(
        WitnessExecutionMode::VerifierReplay,
        Some(leader),
        EnclaveWitnessTapeV1 {
            prefix: staged,
            txs: Vec::new(),
        },
    )));
    let scope = WitnessExecutionScope::enter(replay.clone());
    let replayed = consensus_witness_call(
        CallMeta::recorded("tribute-schema-pin"),
        |_| -> Result<T, TransportError> { panic!("replay executed") },
        accept_frozen_tag,
    )
    .unwrap();
    assert_eq!(replayed, value);
    assert_eq!(WitnessDomainWire::attestation_tag(&replayed), &[81, 82]);
    assert_eq!(replay.lock().unwrap().prefix_cursor, 1);
    drop(scope);
}

#[test]
fn production_schemas_pin_the_frozen_payload() {
    let mut pinned = 0u8;
    for &(index, json, encoded) in vectors() {
        match index {
            7 => pin_production_wire::<NodEncryptedForTributeV1>(json, encoded),
            11 => pin_production_wire::<EncryptedTributeOfferBatchV2>(json, encoded),
            12 => pin_production_wire::<TributeAmountsReadV2>(json, encoded),
            13 => pin_production_wire::<TributeDayOpAppliedV2>(json, encoded),
            14 => pin_production_wire::<TributeDayAmountReadV2>(json, encoded),
            16 => pin_production_wire::<LysisFractionsExecutedV1>(json, encoded),
            17 => pin_production_wire::<TributeDayFractionsReadV1>(json, encoded),
            18 => pin_production_wire::<NodIssuedFromTributeV1>(json, encoded),
            _ => continue,
        }
        pinned += 1;
    }
    assert_eq!(pinned, 9);
}
