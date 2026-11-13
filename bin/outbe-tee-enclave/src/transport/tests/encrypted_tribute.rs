use outbe_core_common::tribute_encryption::{TributeAmountsV2, TributeContextV2};
use outbe_core_common::wwd_entity_id::WwdEntityId;
use outbe_enclave::protocol::{EnclaveRequest as DomainRequest, EnclaveResponse as DomainResponse};
use outbe_system_common::time::WorldwideDay;
use outbe_tee::protocol::PayloadLane;

use crate::transport::tests::*;

#[test]
fn tribute_v2_commands_require_the_installed_network_key() {
    let keys = EnclaveKeys::new([0x51; 32], Some([0x51; 32])).unwrap();
    let keyless = Arc::new(OnceLock::new());
    for request in [
        DomainRequest::ProcessEncryptedTributeOfferBatchV2 { offers: Vec::new() },
        DomainRequest::ReadTributeAmountsV2 {
            tributes: Vec::new(),
        },
    ] {
        let response = dispatch_domain(request, &keys, &keyless, B256::from(testnet_chain_word()));
        let Err(DomainDispatchError::Error(message)) = response else {
            panic!("keyless command succeeded")
        };
        assert_eq!(message, "no resident network key");
    }
}

#[test]
fn private_reads_use_the_common_key_across_enclave_identities_and_bind_the_response() {
    let secret = [7; 32];
    let group_sig = [0x55; 96];
    let day = WorldwideDay::new(20250115);
    let chain_id = 54322345u64;
    let chain = B256::from(alloy_primitives::U256::from(chain_id));
    let tribute_state_key =
        crate::tribute_encryption::derive_tribute_state_key(&group_sig, chain, 0).unwrap();
    let context = TributeContextV2 {
        chain_id,
        tribute_id: WwdEntityId::from_day_and_digest(day, [0x22; 32]),
        owner: alloy_primitives::Address::repeat_byte(0x33),
        worldwide_day: day,
        issuance_currency: 840,
        reference_currency: 978,
        tribute_price_minor: alloy_primitives::U256::from(2_000_000),
        offer_input_hash: B256::repeat_byte(0x44),
    };
    let expected = TributeAmountsV2 {
        issuance_amount_minor: alloy_primitives::U256::from(100_000_000),
        nominal_amount_minor: alloy_primitives::U256::from(50_000_000),
    };
    let encrypted =
        crate::tribute_encryption::encrypt_tribute(&tribute_state_key, context, &expected).unwrap();
    for seed in [0x51, 0x61] {
        let keys = EnclaveKeys::new([seed; 32], Some([seed; 32])).unwrap();
        let (offer_key, _) = install_tribute_offer_key(secret, group_sig.to_vec());
        let response = dispatch_domain(
            DomainRequest::ReadTributeAmountsV2 {
                tributes: vec![encrypted.clone()],
            },
            &keys,
            &offer_key,
            B256::from(alloy_primitives::U256::from(chain_id)),
        );
        let Ok(DomainResponse::TributeAmountsReadV2 {
            amounts,
            inputs_canonical_hash,
            attestation_tag,
        }) = response
        else {
            panic!("installed-key read failed: {response:?}")
        };
        assert_eq!(amounts, vec![expected.clone()]);
        assert_eq!(
            inputs_canonical_hash,
            outbe_enclave::tribute_v2::tribute_read_inputs_hash(std::slice::from_ref(&encrypted))
                .unwrap()
        );
        let preimage = outbe_enclave::tribute_v2::tribute_read_attestation_preimage(
            inputs_canonical_hash,
            &amounts,
        )
        .unwrap();
        outbe_enclave::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &preimage,
            &attestation_tag,
        )
        .unwrap();
        let mut changed = preimage;
        changed[0] ^= 1;
        assert!(outbe_enclave::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &changed,
            &attestation_tag
        )
        .is_err());
        let wrong_chain = dispatch_domain(
            DomainRequest::ReadTributeAmountsV2 {
                tributes: vec![encrypted.clone()],
            },
            &keys,
            &offer_key,
            B256::from(alloy_primitives::U256::from(chain_id + 1)),
        );
        assert!(matches!(wrong_chain, Err(DomainDispatchError::Error(_))));
    }
}

#[test]
fn private_tribute_read_is_denied_to_remote_node_sessions_and_keyless_local_nodes() {
    let enclave = Enclave::new(0x51);
    let request = DomainRequest::ReadTributeAmountsV2 {
        tributes: Vec::new(),
    }
    .encode_wire(PayloadLane::Execution)
    .unwrap();
    assert!(enclave
        .initialization
        .authorize_command(&request, false, SessionAuthorityV1::LocalNodeHost)
        .is_err());
    assert!(enclave
        .initialization
        .authorize_command(&request, true, SessionAuthorityV1::LocalNodeHost)
        .is_ok());
    assert!(enclave
        .initialization
        .authorize_command(
            &request,
            true,
            SessionAuthorityV1::RemoteActiveNode { deadline: u64::MAX }
        )
        .is_err());
}

fn day_request() -> outbe_enclave::tribute_day::TributeDayOpRequestV2 {
    outbe_enclave::tribute_day::TributeDayOpRequestV2 {
        chain_id: 54322345,
        worldwide_day: WorldwideDay::new(20250115),
        previous: None,
        public_state_hash: B256::repeat_byte(0x81),
        operation: outbe_enclave::tribute_day::TributeDayOperationV2::AdjustTransient {
            nominal_amount_minor: alloy_primitives::U256::from(71),
            add: true,
        },
    }
}

#[test]
fn private_day_commands_require_ready_local_authority_and_installed_network_key() {
    let enclave = Enclave::new(0x51);
    let record =
        crate::tribute_day::apply_day_operation(&[7; 32], &[8; 32], &day_request()).unwrap();
    for request in [
        DomainRequest::ApplyTributeDayOpV2 {
            request: Box::new(day_request()),
        },
        DomainRequest::ReadTributeDayAmountV2 { record },
    ] {
        let wire_req = request.encode_wire(PayloadLane::Execution).unwrap();
        assert!(enclave
            .initialization
            .authorize_command(&wire_req, false, SessionAuthorityV1::LocalNodeHost)
            .is_err());
        assert!(enclave
            .initialization
            .authorize_command(&wire_req, true, SessionAuthorityV1::LocalNodeHost)
            .is_ok());
        assert!(enclave
            .initialization
            .authorize_command(
                &wire_req,
                true,
                SessionAuthorityV1::RemoteActiveNode { deadline: u64::MAX }
            )
            .is_err());
        let response = dispatch_domain(
            request,
            &enclave.keys,
            &Arc::new(OnceLock::new()),
            B256::from(testnet_chain_word()),
        );
        assert!(
            matches!(response, Err(DomainDispatchError::Error(message)) if message == "no resident network key")
        );
    }
}

#[test]
fn day_transforms_and_private_reads_are_signed_and_bound_to_the_resident_chain() {
    let request = day_request();
    let secret = [7; 32];
    let mut previous_output = None;
    for seed in [0x51, 0x61] {
        let keys = EnclaveKeys::new([seed; 32], Some([seed; 32])).unwrap();
        let (offer_key, _) = install_tribute_offer_key(secret, vec![0x55; 96]);
        let command = DomainRequest::ApplyTributeDayOpV2 {
            request: Box::new(request.clone()),
        };
        let chain = B256::from(alloy_primitives::U256::from(request.chain_id));
        let response = dispatch_domain(command.clone(), &keys, &offer_key, chain);
        let Ok(DomainResponse::TributeDayOpAppliedV2 {
            record,
            inputs_canonical_hash,
            attestation_tag,
        }) = response
        else {
            panic!("day transform failed")
        };
        let expected = outbe_enclave::tribute_day::day_operation_inputs_hash(&request).unwrap();
        assert_eq!(inputs_canonical_hash, expected);
        let preimage =
            outbe_enclave::tribute_day::day_operation_attestation_preimage(expected, &record)
                .unwrap();
        outbe_enclave::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &preimage,
            &attestation_tag,
        )
        .unwrap();
        if let Some(previous) = previous_output {
            assert_eq!(previous, record);
        }
        previous_output = Some(record.clone());
        let read = DomainRequest::ReadTributeDayAmountV2 {
            record: record.clone(),
        };
        let response = dispatch_domain(read.clone(), &keys, &offer_key, chain);
        let Ok(DomainResponse::TributeDayAmountReadV2 {
            amount,
            inputs_canonical_hash,
            attestation_tag,
        }) = response
        else {
            panic!("day read failed")
        };
        assert_eq!(amount, alloy_primitives::U256::from(71));
        assert_eq!(
            inputs_canonical_hash,
            outbe_enclave::tribute_day::day_read_inputs_hash(&record).unwrap()
        );
        let preimage = outbe_enclave::tribute_day::day_read_attestation_preimage(
            inputs_canonical_hash,
            amount,
        );
        outbe_enclave::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &preimage,
            &attestation_tag,
        )
        .unwrap();
        for command in [command, read] {
            assert!(matches!(
                dispatch_domain(
                    command,
                    &keys,
                    &offer_key,
                    B256::from(alloy_primitives::U256::from(request.chain_id + 1))
                ),
                Err(DomainDispatchError::Error(_))
            ));
        }
    }
}
