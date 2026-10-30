use super::*;
use alloy_sol_types::SolEvent;
use outbe_primitives::{
    addresses::{NOD_ADDRESS, TRIBUTE_ADDRESS},
    time::WorldwideDay,
};
use outbe_tee::{
    lysis::{LysisRequest, LysisResult},
    protocol::EnclaveResponse,
    TransportError,
};
use std::cell::Cell;

const WWD: WorldwideDay = WorldwideDay::new(20260920);
const DAY: u32 = 20261014;
pub(crate) const CLOCK: u64 = 1_792_022_400 + 3_600;
const NOMINAL: u64 = 7_654_321;
const LIMIT: u64 = 1_000_000;
thread_local! {
    static IDENTITY: Cell<u8> = const { Cell::new(1) };
    static CORRUPTION: Cell<u8> = const { Cell::new(0) };
}

pub(crate) fn exchange(
    request: &LysisRequest,
) -> std::result::Result<([u8; 32], EnclaveResponse), TransportError> {
    let encoded = serde_json::to_vec(request).unwrap();
    let request: LysisRequest = serde_json::from_slice(&encoded).unwrap();
    let result = outbe_tee_enclave::lysis::execute(
        &outbe_tribute::enclave_client::test_enclave::NETWORK_SECRET,
        &outbe_nod::test_support::NETWORK_SECRET,
        &outbe_fidelity::enclave_client::test_enclave::state_key(),
        &request,
    );
    match result {
        Ok(result) => attest(&request, result),
        Err(outbe_tee_enclave::errors::TeeError::LysisCalculationRejected) => {
            attest_rejection(&request)
        }
        Err(_) => Err(TransportError::UnexpectedResponse),
    }
}

fn attest_rejection(
    request: &LysisRequest,
) -> std::result::Result<([u8; 32], EnclaveResponse), TransportError> {
    let keys =
        outbe_tee_enclave::keys::EnclaveKeys::new([0x31; 32], Some([IDENTITY.get(); 32])).unwrap();
    let inputs = outbe_tee::lysis::inputs_hash(request)?;
    let mut signature = keys
        .sign_attestation(&outbe_tee::lysis::rejection_preimage(inputs))
        .to_vec();
    let mut response_inputs = inputs;
    match CORRUPTION.get() {
        8 => signature[0] ^= 1,
        9 => response_inputs = B256::ZERO,
        10 => signature.clear(),
        11 => {
            signature = keys.sign_attestation(inputs.as_slice()).to_vec();
        }
        _ => {}
    }
    Ok((
        keys.attestation_pub(),
        EnclaveResponse::LysisRejected {
            inputs_canonical_hash: response_inputs,
            attestation_tag: signature,
        },
    ))
}

fn attest(
    request: &LysisRequest,
    mut result: LysisResult,
) -> std::result::Result<([u8; 32], EnclaveResponse), TransportError> {
    let keys =
        outbe_tee_enclave::keys::EnclaveKeys::new([0x31; 32], Some([IDENTITY.get(); 32])).unwrap();
    let inputs = outbe_tee::lysis::inputs_hash(request)?;
    let signed = outbe_tee::lysis::result_preimage(inputs, &result)?;
    let mut signature = keys.sign_attestation(&signed).to_vec();
    match CORRUPTION.get() {
        1 => {
            result.nods[0].nod.terms.owner = Address::repeat_byte(0xfe);
        }
        2 => signature[0] ^= 1,
        3 => {
            result.total_nominal_minor += U256::ONE;
            return signed_response(request, result, &keys);
        }
        4 => {
            result.nods[1] = result.nods[0].clone();
            return signed_response(request, result, &keys);
        }
        5 => {
            result.nods[0].nod.terms.worldwide_day = WorldwideDay::new(20261016);
            return signed_response(request, result, &keys);
        }
        6 => {
            result.nods.pop();
            return signed_response(request, result, &keys);
        }
        7 => {
            result.unused_lysis_limit_minor = request.lysis_limit_minor + U256::ONE;
            return signed_response(request, result, &keys);
        }
        12 => {
            // A valid signature must not authorize a noncanonical target identity.
            result.nods[0].nod.terms.nod_id = request.tributes[0].context.tribute_id.to_u256();
            return signed_response(request, result, &keys);
        }
        _ => {}
    }
    let response = EnclaveResponse::LysisExecuted {
        result: Box::new(result),
        inputs_canonical_hash: inputs,
        attestation_tag: signature,
    };
    let bytes = serde_json::to_vec(&response).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("gratis_load_minor"));
    assert!(!String::from_utf8_lossy(&bytes).contains("nominal_amount_minor"));
    Ok((
        keys.attestation_pub(),
        serde_json::from_slice(&bytes).unwrap(),
    ))
}

fn signed_response(
    request: &LysisRequest,
    result: LysisResult,
    keys: &outbe_tee_enclave::keys::EnclaveKeys,
) -> std::result::Result<([u8; 32], EnclaveResponse), TransportError> {
    let inputs = outbe_tee::lysis::inputs_hash(request)?;
    let signature = keys
        .sign_attestation(&outbe_tee::lysis::result_preimage(inputs, &result)?)
        .to_vec();
    Ok((
        keys.attestation_pub(),
        EnclaveResponse::LysisExecuted {
            result: Box::new(result),
            inputs_canonical_hash: inputs,
            attestation_tag: signature,
        },
    ))
}

fn unavailable(
    _: &LysisRequest,
) -> std::result::Result<([u8; 32], EnclaveResponse), TransportError> {
    Err(TransportError::UnexpectedResponse)
}

pub(crate) fn fixture() -> HashMapStorageProvider {
    fixture_with_amounts(LIMIT, &[NOMINAL + 1, NOMINAL + 2, NOMINAL + 3])
}

fn fixture_with_amounts(limit: u64, amounts: &[u64]) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(10);
    provider.set_timestamp(U256::from(CLOCK));
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 8);
    outbe_tribute::enclave_client::test_enclave::install();
    outbe_fidelity::enclave_client::test_enclave::install();
    StorageHandle::enter(&mut provider, |storage| {
        install_test_enclaves();
        arm_reference_price(&storage, CLOCK);
        let forming_start = WWD.start_timestamp();
        let forming_end =
            forming_start + outbe_chain_constants::get_metadosis_forming_period_seconds();
        let offerings_start =
            forming_end + outbe_chain_constants::get_metadosis_lookback_delay_seconds();
        let offering_end =
            offerings_start + outbe_chain_constants::get_metadosis_offering_period_seconds();
        let binding = outbe_primitives::storage::metadosis_process_ready_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .clone()
            .with_metadosis_mutation::<outbe_primitives::storage::MetadosisCycleLifecycle, _>(
                binding,
                || {
                    let mut factory = TributeFactoryContract::new(storage.clone());
                    let mut tribute = TributeContract::new(storage.clone());
                    let metadosis = MetadosisContract::new(storage.clone());
                    let mut ed = crate::schema::EmissionDay::new_with_limit(
                        DAY,
                        WWD,
                        50,
                        u128::from(limit),
                        emission_day_state::NONE,
                    );
                    ed.state = outbe_emission::schema::emission_day_state::PROCESSING;
                    metadosis.emission_days().create(&ed)?;
                    for (index, amount) in amounts.iter().enumerate() {
                        let owner = Address::repeat_byte(u8::try_from(index + 1).unwrap());
                        let amount = U256::from(*amount);
                        tribute.issue(&outbe_tribute::TributeData {
                            tribute_id: outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(
                                owner, WWD,
                            ),
                            owner,
                            worldwide_day: WWD,
                            issuance_amount_minor: amount,
                            issuance_currency: 840,
                            nominal_amount_minor: amount,
                            reference_currency: 840,
                            tribute_price_minor: U256::from(2),
                        })?;
                    }
                    outbe_tributefactory::worldwideday::create_worldwide_day(
                        &mut factory,
                        WWD,
                        outbe_tributefactory::worldwideday::NewWwdSchedule {
                            day: DAY,
                            forming_start,
                            forming_end,
                            offerings_start,
                            offering_end,
                        },
                    )?;
                    Ok(())
                },
            )
            .unwrap();
    });
    CORRUPTION.set(0);
    provider
}

fn run(provider: &mut HashMapStorageProvider) -> outbe_primitives::error::Result<()> {
    StorageHandle::enter(provider, |storage| {
        crate::commands::start_metadosis(&BlockRuntimeContext::new(
            BlockContext::empty_for_tests(10, CLOCK, CHAIN_ID),
            storage,
        ))
    })
}

#[test]
fn native_fraction_completion_keeps_tributes_for_owner_create_nod() {
    let mut provider = fixture();
    assert!(!outbe_tee::lysis::is_test_transport_installed());

    run(&mut provider).unwrap();

    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis
                .emission_days()
                .get(DAY)
                .unwrap()
                .unwrap()
                .metadosis_state,
            emission_day_state::SUCCEEDED
        );

        let tribute = TributeContract::new(storage.clone());
        assert_eq!(tribute.total_supply().unwrap(), 3);
        assert_eq!(tribute.state_records_by_day(WWD).unwrap().len(), 3);
        assert!(!tribute.retired_days.read(&WWD).unwrap());
        assert_eq!(
            outbe_nod::schema::NodContract::new(storage)
                .total_supply()
                .unwrap(),
            0
        );
    });
}

#[test]
fn native_completion_credits_the_unused_lysis_limit_once() {
    let mut provider = fixture();
    let before = StorageHandle::enter(&mut provider, |storage| {
        outbe_promislimit::PromisLimitContract::new(storage)
            .promis_limit()
            .unwrap()
    });

    run(&mut provider).unwrap();

    let executed = provider
        .get_ordered_events()
        .iter()
        .filter_map(|event| IMetadosis::MetadosisExecuted::decode_log(event).ok())
        .map(|event| event.data)
        .collect::<Vec<_>>();
    assert_eq!(executed.len(), 1);
    assert_eq!(executed[0].status, "COMPLETED");
    let unused = executed[0].unusedLysisLimitMinor;
    assert!(!unused.is_zero());
    StorageHandle::enter(&mut provider, |storage| {
        let emission = MetadosisContract::new(storage.clone())
            .emission_days()
            .get(DAY)
            .unwrap()
            .unwrap()
            .emission_limit_minor();
        let desis = outbe_desis::schema::DesisContract::new(storage.clone())
            .pending_desis_limit_minor
            .read(&WWD)
            .unwrap();
        let after = outbe_promislimit::PromisLimitContract::new(storage)
            .promis_limit()
            .unwrap();
        assert_eq!(
            after + desis,
            before + (emission - executed[0].lysisLimitMinor) + unused
        );
    });

    let storage_before = provider.storage.clone();
    let logs = provider.get_ordered_events().to_vec();
    run(&mut provider).unwrap();
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.get_ordered_events(), logs);
}

#[test]
fn native_completion_briefs_the_emission_day() {
    let mut provider = fixture();
    run(&mut provider).unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let desis = outbe_desis::schema::DesisContract::new(storage);
        assert_eq!(desis.brief_emission_day.read(&WWD).unwrap(), DAY);
    });
}

#[test]
fn production_command_issues_encrypted_nods_and_completes_without_worker() {
    let _transport = outbe_tee::lysis::test_transport::scope(exchange);
    // Issuing a Lysis Nod reads its Gratis load for the group's unpaid total.
    let _nod_enclave = outbe_nod::test_support::scope();
    let mut provider = fixture();
    run(&mut provider).unwrap();
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            metadosis
                .emission_days()
                .get(DAY)
                .unwrap()
                .unwrap()
                .metadosis_state,
            emission_day_state::SUCCEEDED
        );
        let tribute = TributeContract::new(storage.clone());
        assert_eq!(tribute.total_supply().unwrap(), 0);
        assert!(tribute.retired_days.read(&WWD).unwrap());
        let nods = outbe_nod::api::list_by_owner(&storage, Address::repeat_byte(1)).unwrap();
        assert_eq!(nods.len(), 1);
        let owner = Address::repeat_byte(1);
        assert_eq!(nods[0].series_id.day(), DAY);
        assert_eq!(
            nods[0].nod_id,
            outbe_primitives::nod_rules::derive_nod_id(nods[0].series_id, owner)
        );
        assert_ne!(
            nods[0].nod_id,
            outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(owner, WWD).to_u256()
        );
        let load = outbe_tee_enclave::nod_encryption::decrypt_nod(&[0x5a; 32], &nods[0].encrypted)
            .unwrap();
        assert_eq!(load, U256::from(LIMIT / 1024));
    });
    let before = provider.storage.clone();
    let logs = provider.get_ordered_events().to_vec();
    run(&mut provider).unwrap();
    assert_eq!(provider.storage, before);
    assert_eq!(provider.get_ordered_events(), logs);
}

#[test]
fn local_identity_does_not_change_ciphertexts_state_or_events() {
    let _transport = outbe_tee::lysis::test_transport::scope(exchange);
    // Issuing a Lysis Nod reads its Gratis load for the group's unpaid total.
    let _nod_enclave = outbe_nod::test_support::scope();
    let mut first = fixture();
    let mut second = fixture();
    first.enable_production_storage_gas_metering();
    second.enable_production_storage_gas_metering();
    IDENTITY.set(1);
    run(&mut first).unwrap();
    IDENTITY.set(2);
    run(&mut second).unwrap();
    IDENTITY.set(1);
    assert_eq!(first.storage, second.storage);
    assert_eq!(first.get_ordered_events(), second.get_ordered_events());
    assert_eq!(
        outbe_primitives::storage::PrecompileStorageProvider::gas_used(&first),
        outbe_primitives::storage::PrecompileStorageProvider::gas_used(&second),
    );
}

#[test]
fn existing_full_digest_target_rejects_lysis_before_mutation() {
    let _transport = outbe_tee::lysis::test_transport::scope(exchange);
    let _nod_enclave = outbe_nod::test_support::scope();
    let mut provider = fixture();
    StorageHandle::enter(&mut provider, |storage| {
        let mut item = outbe_nod::test_support::sample_item(Address::repeat_byte(1), WWD);
        let terms =
            outbe_nod::test_support::sealed_terms(&storage, &item, item.entry_price_minor())
                .unwrap();
        item.group_key = outbe_nod::identity::group_key(
            item.issued_at,
            terms.call_price_minor,
            item.reference_currency,
        )
        .unwrap();
        outbe_nod::test_support::add(&storage, &item).unwrap();
    });
    let before = provider.storage.clone();
    let events = provider.get_ordered_events().to_vec();
    assert!(run(&mut provider).is_err());
    assert_eq!(provider.storage, before);
    assert_eq!(provider.get_ordered_events(), events);
}

#[test]
fn failed_enclave_and_invalid_results_leave_all_state_unchanged() {
    for fault in (0..=7).chain([12]) {
        let mut provider = fixture();
        CORRUPTION.set(fault);
        let before = provider.storage.clone();
        let events = provider.get_ordered_events().to_vec();
        let _transport = outbe_tee::lysis::test_transport::scope(if fault == 0 {
            unavailable
        } else {
            exchange
        });
        assert!(run(&mut provider).is_err());
        assert_eq!(provider.storage, before);
        assert_eq!(provider.get_ordered_events(), events);
    }
    CORRUPTION.set(0);
}

#[test]
fn calculation_rejection_only_fails_the_day_and_preserves_all_economic_state() {
    let _transport = outbe_tee::lysis::test_transport::scope(exchange);
    for (limit, amounts) in [(1, vec![NOMINAL, NOMINAL + 1]), (LIMIT, vec![1, NOMINAL])] {
        let mut provider = fixture_with_amounts(limit, &amounts);
        run(&mut provider).unwrap();
        StorageHandle::enter(&mut provider, |storage| {
            let metadosis = MetadosisContract::new(storage.clone());
            assert_eq!(
                metadosis
                    .emission_days()
                    .get(DAY)
                    .unwrap()
                    .unwrap()
                    .metadosis_state,
                emission_day_state::FAILED
            );
            let tribute = TributeContract::new(storage.clone());
            assert_eq!(tribute.total_supply().unwrap(), amounts.len() as u64);
            assert!(!tribute.retired_days.read(&WWD).unwrap());
            assert!(tribute.get_day_totals(WWD).unwrap().is_sealed);
            assert_eq!(
                outbe_nod::schema::NodContract::new(storage)
                    .total_supply()
                    .unwrap(),
                0
            );
        });
        let after = provider.storage.clone();
        let events = provider.get_ordered_events().to_vec();
        run(&mut provider).unwrap();
        assert_eq!(provider.storage, after);
        assert_eq!(provider.get_ordered_events(), events);
    }
}

#[test]
fn unauthenticated_rejection_and_local_fault_leave_ready_then_allow_retry() {
    let mut control = fixture_with_amounts(1, &[NOMINAL]);
    let _transport = outbe_tee::lysis::test_transport::scope(exchange);
    run(&mut control).unwrap();
    for fault in 0..=4 {
        let mut provider = fixture_with_amounts(1, &[NOMINAL]);
        let before = provider.storage.clone();
        let events = provider.get_ordered_events().to_vec();
        CORRUPTION.set(fault + 7);
        {
            let _transport = outbe_tee::lysis::test_transport::scope(if fault == 0 {
                unavailable
            } else {
                exchange
            });
            assert!(matches!(
                run(&mut provider),
                Err(outbe_primitives::error::PrecompileError::EnclaveUnavailable(_))
            ));
        }
        assert_eq!(provider.storage, before);
        assert_eq!(provider.get_ordered_events(), events);
        CORRUPTION.set(0);
        run(&mut provider).unwrap();
        assert_eq!(provider.storage, control.storage);
        assert_eq!(provider.get_ordered_events(), control.get_ordered_events());
    }
}

#[test]
fn rejected_day_status_and_event_writes_roll_back_and_retry_exactly() {
    let _transport = outbe_tee::lysis::test_transport::scope(exchange);
    let mut control = fixture_with_amounts(1, &[NOMINAL]);
    control.fail_after_mutation_at(usize::MAX);
    run(&mut control).unwrap();
    let operations = control.clear_mutation_failure();
    assert!(operations > 0);
    for operation in 0..operations {
        let mut provider = fixture_with_amounts(1, &[NOMINAL]);
        let before = provider.storage.clone();
        let events = provider.get_ordered_events().to_vec();
        provider.fail_after_mutation_at(operation);
        assert!(run(&mut provider).is_err(), "operation {operation}");
        provider.clear_mutation_failure();
        assert_eq!(provider.storage, before, "operation {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            events,
            "operation {operation}"
        );
        run(&mut provider).unwrap();
        assert_eq!(provider.storage, control.storage, "retry {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            control.get_ordered_events(),
            "retry {operation}"
        );
    }
}

#[test]
fn failed_owner_writes_roll_back_nods_tribute_and_economics() {
    let _transport = outbe_tee::lysis::test_transport::scope(exchange);
    for address in [NOD_ADDRESS, TRIBUTE_ADDRESS] {
        let mut provider = fixture();
        let before = provider.storage.clone();
        let events = provider.get_ordered_events().to_vec();
        provider.fail_mutation_at_address(address);
        assert!(run(&mut provider).is_err());
        provider.clear_mutation_failure();
        assert_eq!(provider.storage, before);
        assert_eq!(provider.get_ordered_events(), events);
    }
}

#[test]
fn tribute_consumption_requires_the_exact_metadosis_command() {
    let mut provider = fixture();
    StorageHandle::enter(&mut provider, |storage| {
        let mut tribute = TributeContract::new(storage.clone());
        let nominal = tribute
            .get_day_totals(WWD)
            .unwrap()
            .tribute_nominal_total_minor;
        assert!(tribute
            .consume_and_retire_lysis_partition(WWD, 3, nominal)
            .is_err());
        let wrong = outbe_primitives::storage::metadosis_init_genesis_binding(CHAIN_ID, 10, CLOCK);
        storage
            .with_metadosis_mutation::<outbe_primitives::storage::MetadosisCycleLifecycle, _>(
                wrong,
                || {
                    assert!(tribute
                        .consume_and_retire_lysis_partition(WWD, 3, nominal)
                        .is_err());
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(tribute.total_supply().unwrap(), 3);
    });
}
