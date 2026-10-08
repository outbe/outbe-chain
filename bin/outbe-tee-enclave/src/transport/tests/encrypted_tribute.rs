use crate::transport::tests::*;
use outbe_primitives::{
    time::WorldwideDay,
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
    wwd_entity_id::WwdEntityId,
};

#[test]
fn tribute_v2_commands_require_the_installed_network_key() {
    let keys = EnclaveKeys::new([0x51; 32], Some([0x51; 32])).unwrap();
    let mut dkg = DkgSessionStore::new();
    let keyless = Arc::new(OnceLock::new());
    for request in [
        EnclaveRequest::ProcessEncryptedTributeOfferBatchV2 { offers: Vec::new() },
        EnclaveRequest::ReadTributeAmountsV2 {
            tributes: Vec::new(),
        },
    ] {
        let response = dispatch(
            request,
            &keys,
            &mut dkg,
            &keyless,
            B256::from(testnet_chain_word()),
        );
        let EnclaveResponse::Error { message } = response else {
            panic!("keyless command succeeded")
        };
        assert_eq!(message, "no resident network key");
    }
}

#[test]
fn private_reads_use_the_common_key_across_enclave_identities_and_bind_the_response() {
    let secret = [7; 32];
    let creator_public = crate::crypto::x25519_public(&[11; 32]);
    let day = WorldwideDay::new(20250115);
    let chain_id = 54322345u64;
    let context = TributeContextV2 {
        chain_id,
        tribute_id: WwdEntityId::from_day_and_digest(day, [0x22; 32]),
        owner: alloy_primitives::Address::repeat_byte(0x33),
        worldwide_day: day,
        issuance_currency: 840,
        reference_currency: 978,
        tribute_price_minor: alloy_primitives::U256::from(2_000_000),
        exclude_from_intex_issuance: false,
        offer_input_hash: B256::repeat_byte(0x44),
    };
    let expected = TributeAmountsV2 {
        issuance_amount_minor: alloy_primitives::U256::from(100_000_000),
        nominal_amount_minor: alloy_primitives::U256::from(50_000_000),
    };
    let encrypted =
        crate::tribute_encryption::encrypt_tribute(&secret, &creator_public, context, &expected)
            .unwrap();
    for seed in [0x51, 0x61] {
        let keys = EnclaveKeys::new([seed; 32], Some([seed; 32])).unwrap();
        let (offer_key, _) = install_tribute_offer_key(secret, vec![0x55; 96]);
        let response = dispatch(
            EnclaveRequest::ReadTributeAmountsV2 {
                tributes: vec![encrypted.clone()],
            },
            &keys,
            &mut DkgSessionStore::new(),
            &offer_key,
            B256::from(alloy_primitives::U256::from(chain_id)),
        );
        let EnclaveResponse::TributeAmountsReadV2 {
            amounts,
            inputs_canonical_hash,
            attestation_tag,
        } = response
        else {
            panic!("installed-key read failed: {response:?}")
        };
        assert_eq!(amounts, vec![expected.clone()]);
        assert_eq!(
            inputs_canonical_hash,
            outbe_tee::tribute_v2::tribute_read_inputs_hash(std::slice::from_ref(&encrypted))
                .unwrap()
        );
        let preimage = outbe_tee::tribute_v2::tribute_read_attestation_preimage(
            inputs_canonical_hash,
            &amounts,
        )
        .unwrap();
        outbe_tee::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &preimage,
            &attestation_tag,
        )
        .unwrap();
        let mut changed = preimage;
        changed[0] ^= 1;
        assert!(outbe_tee::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &changed,
            &attestation_tag
        )
        .is_err());
        let wrong_chain = dispatch(
            EnclaveRequest::ReadTributeAmountsV2 {
                tributes: vec![encrypted.clone()],
            },
            &keys,
            &mut DkgSessionStore::new(),
            &offer_key,
            B256::from(alloy_primitives::U256::from(chain_id + 1)),
        );
        assert!(matches!(wrong_chain, EnclaveResponse::Error { .. }));
    }
}

#[test]
fn private_tribute_read_is_denied_to_remote_node_sessions_and_keyless_local_nodes() {
    let enclave = Enclave::new(0x51);
    let request = EnclaveRequest::ReadTributeAmountsV2 {
        tributes: Vec::new(),
    };
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

fn day_request() -> outbe_tee::tribute_day::TributeDayOpRequestV2 {
    outbe_tee::tribute_day::TributeDayOpRequestV2 {
        chain_id: 54322345,
        worldwide_day: WorldwideDay::new(20250115),
        previous: None,
        public_state_hash: B256::repeat_byte(0x81),
        operation: outbe_tee::tribute_day::TributeDayOperationV2::AdjustTransient {
            nominal_amount_minor: alloy_primitives::U256::from(71),
            add: true,
        },
    }
}

#[test]
fn private_day_commands_require_ready_local_authority_and_installed_network_key() {
    let enclave = Enclave::new(0x51);
    let record = crate::tribute_day::apply_day_operation(&[7; 32], &day_request()).unwrap();
    for request in [
        EnclaveRequest::ApplyTributeDayOpV2 {
            request: Box::new(day_request()),
        },
        EnclaveRequest::ReadTributeDayAmountV2 { record },
    ] {
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
        let response = dispatch(
            request,
            &enclave.keys,
            &mut DkgSessionStore::new(),
            &Arc::new(OnceLock::new()),
            B256::from(testnet_chain_word()),
        );
        assert!(
            matches!(response, EnclaveResponse::Error { message } if message == "no resident network key")
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
        let command = EnclaveRequest::ApplyTributeDayOpV2 {
            request: Box::new(request.clone()),
        };
        let chain = B256::from(alloy_primitives::U256::from(request.chain_id));
        let response = dispatch(
            command.clone(),
            &keys,
            &mut DkgSessionStore::new(),
            &offer_key,
            chain,
        );
        let EnclaveResponse::TributeDayOpAppliedV2 {
            record,
            inputs_canonical_hash,
            attestation_tag,
        } = response
        else {
            panic!("day transform failed")
        };
        let expected = outbe_tee::tribute_day::day_operation_inputs_hash(&request).unwrap();
        assert_eq!(inputs_canonical_hash, expected);
        let preimage =
            outbe_tee::tribute_day::day_operation_attestation_preimage(expected, &record).unwrap();
        outbe_tee::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &preimage,
            &attestation_tag,
        )
        .unwrap();
        if let Some(previous) = previous_output {
            assert_eq!(previous, record);
        }
        previous_output = Some(record.clone());
        let read = EnclaveRequest::ReadTributeDayAmountV2 {
            record: record.clone(),
        };
        let response = dispatch(
            read.clone(),
            &keys,
            &mut DkgSessionStore::new(),
            &offer_key,
            chain,
        );
        let EnclaveResponse::TributeDayAmountReadV2 {
            amount,
            inputs_canonical_hash,
            attestation_tag,
        } = response
        else {
            panic!("day read failed")
        };
        assert_eq!(amount, alloy_primitives::U256::from(71));
        assert_eq!(
            inputs_canonical_hash,
            outbe_tee::tribute_day::day_read_inputs_hash(&record).unwrap()
        );
        let preimage =
            outbe_tee::tribute_day::day_read_attestation_preimage(inputs_canonical_hash, amount);
        outbe_tee::tribute_v2::verify_attestation(
            &keys.attestation_pub(),
            &preimage,
            &attestation_tag,
        )
        .unwrap();
        for command in [command, read] {
            assert!(matches!(
                dispatch(
                    command,
                    &keys,
                    &mut DkgSessionStore::new(),
                    &offer_key,
                    B256::from(alloy_primitives::U256::from(request.chain_id + 1))
                ),
                EnclaveResponse::Error { .. }
            ));
        }
    }
}
