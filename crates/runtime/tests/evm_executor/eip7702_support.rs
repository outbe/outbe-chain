use super::*;

use outbe_system_common::addresses::{AGENT_REWARD_ADDRESS, ZEROFEE_ADDRESS};

pub(super) fn agent_reward_query_input() -> Vec<u8> {
    let selector = keccak256(b"getClaimableBalance(address)");
    let mut input = Vec::with_capacity(36);
    input.extend_from_slice(&selector[..4]);
    input.extend_from_slice(&[0u8; 32]);
    input
}
/// Sponsored signer derived from the alloy test-signature recovery.
/// We don't care WHICH address it is. We only need it to be stable across
/// runs, and we attach delegation + balance + nonce to it.
pub(super) fn sponsored_test_tx(input: Vec<u8>) -> reth_ethereum::TransactionSigned {
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 200_000,
        max_fee_per_gas: alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(AGENT_REWARD_ADDRESS),
        value: U256::ZERO,
        input: input.into(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}
/// CfgEnv configured for Pectra (EIP-7702-active). The default test
/// cfg uses SHANGHAI, which silently disables delegation re-load.
pub(super) fn pectra_evm_env(block_number: u64) -> EvmEnv {
    EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
        block_env: BlockEnv {
            number: U256::from(block_number),
            gas_limit: 30_000_000,
            basefee: alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE,
            beneficiary: OWNER,
            // 2026-04-01 00:00:00 UTC - matches BLOCK_DAY constant
            // in the zerofee unit tests for cross-reference.
            timestamp: U256::from(1_775_001_600u64),
            ..Default::default()
        },
    }
}
pub(super) fn sign_test_hash(
    key: &k256::ecdsa::SigningKey,
    hash: &B256,
) -> alloy_primitives::Signature {
    let (signature, recovery_id): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = key
        .sign_prehash(hash.as_slice())
        .expect("test prehash signing must succeed");
    alloy_primitives::Signature::from_bytes_and_parity(
        signature.to_bytes().as_slice(),
        recovery_id.to_byte() != 0,
    )
    .normalized_s()
}
pub(super) fn bootstrap_test_tx(
) -> reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned> {
    let key = k256::ecdsa::SigningKey::from_slice(&[0x11; 32]).unwrap();
    let signer = Address::from_public_key(key.verifying_key());
    let authorization = Authorization {
        chain_id: U256::from(CHAIN_ID),
        address: ZEROFEE_ADDRESS,
        nonce: 1,
    };
    let authorization_signature = sign_test_hash(&key, &authorization.signature_hash());
    let signed_authorization = authorization.into_signed(authorization_signature);
    let input = outbe_zerofee::precompile::IZeroFee::authorizeSponsorshipCall { signer }
        .abi_encode()
        .into();
    let tx = TxEip7702 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: outbe_zerofee::FREE_TX_BOOTSTRAP_GAS_LIMIT,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: ZEROFEE_ADDRESS,
        value: U256::ZERO,
        access_list: Default::default(),
        authorization_list: vec![signed_authorization],
        input,
    };
    let tx_signature = sign_test_hash(&key, &tx.signature_hash());
    let recovered = reth_ethereum::TransactionSigned::from(tx.into_signed(tx_signature))
        .try_into_recovered()
        .expect("bootstrap signer must recover");
    assert_eq!(Address::from(*recovered.signer()), signer);
    recovered
}

pub(super) fn cache_db_with_paymaster_account(
    signer: Address,
    signer_balance: U256,
) -> CacheDB<EmptyDBTyped<ProviderError>> {
    let mut db = CacheDB::<EmptyDBTyped<ProviderError>>::default();

    // ZEROFEE_ADDRESS: marker bytecode for EIP-161 preservation.
    let marker = Bytecode::new_legacy([0xef].into());
    db.insert_account_info(
        ZEROFEE_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker.clone()),
            ..Default::default()
        },
    );
    // AGENT_REWARD_ADDRESS: same marker, it is a precompile target.
    db.insert_account_info(
        AGENT_REWARD_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );

    // signer: EIP-7702 delegated to ZEROFEE_ADDRESS, with the
    // requested balance so sponsored fee-debit invariants can be
    // exercised for both funded and exactly-zero accounts.
    let delegation = Bytecode::new_eip7702(ZEROFEE_ADDRESS);
    db.insert_account_info(
        signer,
        AccountInfo {
            balance: signer_balance,
            code_hash: delegation.hash_slow(),
            code: Some(delegation),
            ..Default::default()
        },
    );
    db
}

pub(super) fn zerofee_counter_for(
    state: &mut State<CacheDB<EmptyDBTyped<ProviderError>>>,
    signer: Address,
) -> u64 {
    // Reconstruct the counter slot via the same Map<Address, u64>
    // the contract uses, then read it directly off the bundle
    // state as a raw U256 and narrow to u64.
    let mut slot_storage = HashMapStorageProvider::new(CHAIN_ID);
    let slot = StorageHandle::enter(&mut slot_storage, |storage| {
        outbe_zerofee::ZeroFeeContract::new(storage.clone()).counter_slot(signer)
    });
    state
        .bundle_state
        .storage(&ZEROFEE_ADDRESS, slot)
        .unwrap_or_default()
        .saturating_to::<u64>()
}

/// Computes the ZEROFEE counter storage slot for `signer` (the same
/// keccak-derived `Map<Address,u64>` slot the contract uses).
pub(super) fn zerofee_counter_slot(signer: Address) -> U256 {
    let mut slot_storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut slot_storage, |storage| {
        outbe_zerofee::ZeroFeeContract::new(storage.clone()).counter_slot(signer)
    })
}
