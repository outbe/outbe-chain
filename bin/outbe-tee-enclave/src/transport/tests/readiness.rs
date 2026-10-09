use crate::initialization::CommandDenial;
use crate::transport::tests::*;
use alloy_primitives::{Address, U256};
use outbe_primitives::{
    nod_encryption::{EncryptedNodV2, NodTermsV2},
    time::WorldwideDay,
    wwd_entity_id::WwdEntityId,
};

fn encrypted_nod() -> EncryptedNodV2 {
    let day = WorldwideDay::new(20250115);
    let terms = NodTermsV2 {
        chain_id: 54322345,
        nod_id: WwdEntityId::from_day_and_digest(day, B256::repeat_byte(6)),
        owner: Address::repeat_byte(3),
        worldwide_day: day,
        league_id: 0,
        entry_price_minor: U256::from(700),
        issuance_currency: 840,
        reference_currency: 978,
    };
    crate::nod_encryption::encrypt_nod(&[7; 32], &[11; 32], terms, U256::from(123)).unwrap()
}

#[test]
fn a_keyless_enclave_or_an_expired_lease_is_not_ready() {
    let enclave = Enclave::new(0x51);
    let read = EnclaveRequest::ReadNodAmountV2 {
        nod: encrypted_nod(),
    };
    assert_eq!(
        enclave
            .initialization
            .authorize_command(&read, false, SessionAuthorityV1::LocalNodeHost),
        Err(CommandDenial::NotReady(
            "command denied by enclave state matrix"
        ))
    );
    assert_eq!(
        enclave.initialization.authorize_command(
            &EnclaveRequest::GetPublicKeys,
            true,
            SessionAuthorityV1::RemoteActiveNode { deadline: 0 }
        ),
        Err(CommandDenial::NotReady("remote session lease expired"))
    );
    let response = dispatch(
        read,
        &enclave.keys,
        &mut DkgSessionStore::new(),
        &Arc::new(OnceLock::new()),
        B256::from(testnet_chain_word()),
    );
    assert!(
        matches!(response, EnclaveResponse::NotReady { message } if message == "no resident network key")
    );
}

#[test]
fn a_command_this_state_never_allows_is_forbidden() {
    let enclave = Enclave::new(0x51);
    for offer_key_ready in [false, true] {
        assert!(matches!(
            enclave.initialization.authorize_command(
                &EnclaveRequest::GetQuote { nonce: [0; 32] },
                offer_key_ready,
                SessionAuthorityV1::LocalNodeHost
            ),
            Err(CommandDenial::Forbidden(_))
        ));
    }
    let founding = EnclaveRequest::DkgOpen {
        ceremony_id: B256::repeat_byte(1),
        round: 0,
        participants: Vec::new(),
    };
    assert!(enclave
        .initialization
        .authorize_command(&founding, false, SessionAuthorityV1::LocalNodeHost)
        .is_ok());
    assert!(matches!(
        enclave.initialization.authorize_command(
            &founding,
            true,
            SessionAuthorityV1::LocalNodeHost
        ),
        Err(CommandDenial::Forbidden(_))
    ));
    assert!(matches!(
        enclave.initialization.authorize_command(
            &EnclaveRequest::ReadNodAmountV2 {
                nod: encrypted_nod()
            },
            true,
            SessionAuthorityV1::RemoteActiveNode { deadline: u64::MAX }
        ),
        Err(CommandDenial::Forbidden(_))
    ));
}
