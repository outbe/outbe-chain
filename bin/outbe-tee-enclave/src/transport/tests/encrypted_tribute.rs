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
            outbe_tee::tribute_v2::tribute_read_inputs_hash(&[encrypted.clone()]).unwrap()
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
