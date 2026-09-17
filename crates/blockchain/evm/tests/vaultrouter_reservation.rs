//! Real token balances, allowances and vault shares across precompile frames.
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_compressed_entities::ExecutionScope;
use outbe_evm::sub_call;
use outbe_primitives::{
    addresses::{CREDIS_FACTORY_ADDRESS, GRATIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS},
    block::BlockContext,
    storage::{direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallStatus},
};
use outbe_vaultrouter::{api::IVaultRouter, VaultRouterContract};
use revm::{
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Context,
};

sol! {
    interface Token {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function setFee(bool enabled) external;
        function balanceOf(address account) external view returns (uint256);
        function allowance(address account, address spender) external view returns (uint256);
    }
    interface Vault {
        function configure(address token, address router, uint256 shares) external;
        function setRejectDeposits(bool reject) external;
    }
}

const ASSET: Address = Address::new([0x33; 20]);
const VAULT: Address = Address::new([0x55; 20]);
const OTHER_VAULT: Address = Address::new([0x56; 20]);
const RECEIVER: Address = Address::new([0x77; 20]);
const ID: B256 = B256::repeat_byte(1);
type TestContext =
    Context<revm::context::BlockEnv, revm::context::TxEnv, revm::context::CfgEnv, CacheDB<EmptyDB>>;

fn call(
    ctx: &mut TestContext,
    caller: Address,
    target: Address,
    calldata: Vec<u8>,
) -> outbe_primitives::storage::SubCallOutput {
    sub_call::run(
        ctx,
        caller,
        false,
        SpecId::PRAGUE,
        None,
        Arc::new(ExecutionScope::new()),
        SubCallInput {
            target,
            value: U256::ZERO,
            calldata: calldata.into(),
            gas_limit: 10_000_000,
            is_static: false,
        },
    )
    .unwrap()
}

fn ok_call(ctx: &mut TestContext, caller: Address, target: Address, calldata: Vec<u8>) -> Bytes {
    let out = call(ctx, caller, target, calldata);
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "{:?}: {:?}",
        out.status,
        out.returndata
    );
    out.returndata
}

fn setup() -> TestContext {
    let mut db = CacheDB::new(EmptyDB::default());
    for (address, code) in [
        (
            ASSET,
            include_str!("fixtures/reservation/ReservationToken.hex"),
        ),
        (
            VAULT,
            include_str!("fixtures/reservation/ReservationVault.hex"),
        ),
        (
            OTHER_VAULT,
            include_str!("fixtures/reservation/ReservationVault.hex"),
        ),
        (
            RECEIVER,
            include_str!("fixtures/reservation/ReservationReceiver.hex"),
        ),
    ] {
        let code = Bytecode::new_raw(alloy_primitives::hex::decode(code.trim()).unwrap().into());
        db.insert_account_info(
            address,
            AccountInfo {
                code_hash: code.hash_slow(),
                code: Some(code),
                ..Default::default()
            },
        );
    }
    let block = BlockContext::new(
        1,
        1000,
        outbe_primitives::chain::CHAIN_ID,
        RECEIVER,
        vec![RECEIVER],
    );
    let mut provider = DirectStorageProvider::new(&mut db, block);
    StorageHandle::enter(&mut provider, |storage| {
        let router = VaultRouterContract::new(storage);
        router.asset_vault_set(ASSET).insert(VAULT).unwrap();
        router
            .liquidity_sources
            .insert(CREDIS_FACTORY_ADDRESS)
            .unwrap();
        router
            .liquidity_source_types
            .write(&CREDIS_FACTORY_ADDRESS, 2)
            .unwrap();
        router.asset_vault_set(ASSET).insert(OTHER_VAULT).unwrap();
    });
    provider.flush().unwrap();
    let mut ctx = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| cfg.chain_id = outbe_primitives::chain::CHAIN_ID)
        .modify_block_chained(|block| block.timestamp = U256::from(1000));
    ok_call(
        &mut ctx,
        RECEIVER,
        ASSET,
        Token::mintCall {
            to: VAULT,
            amount: U256::from(1000),
        }
        .abi_encode(),
    );
    ok_call(
        &mut ctx,
        RECEIVER,
        VAULT,
        Vault::configureCall {
            token: ASSET,
            router: VAULT_ROUTER_ADDRESS,
            shares: U256::from(1000),
        }
        .abi_encode(),
    );
    ctx
}

fn reserve(ctx: &mut TestContext) {
    ok_call(
        ctx,
        GRATIS_FACTORY_ADDRESS,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::reserveCall {
            id: ID,
            asset: ASSET,
            amount: U256::from(100),
            validUntil: 1900,
        }
        .abi_encode(),
    );
}

fn balance(ctx: &mut TestContext, account: Address) -> U256 {
    let bytes = ok_call(
        ctx,
        RECEIVER,
        ASSET,
        Token::balanceOfCall { account }.abi_encode(),
    );
    Token::balanceOfCall::abi_decode_returns(&bytes).unwrap()
}

fn reserved(ctx: &mut TestContext) -> U256 {
    let bytes = ok_call(
        ctx,
        RECEIVER,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::reservedTotalCall { asset: ASSET }.abi_encode(),
    );
    IVaultRouter::reservedTotalCall::abi_decode_returns(&bytes).unwrap()
}

#[test]
fn reserve_and_release_transfer_real_tokens_once_and_clear_approval() {
    let mut ctx = setup();
    reserve(&mut ctx);
    assert_eq!(balance(&mut ctx, VAULT), U256::from(900));
    assert_eq!(balance(&mut ctx, VAULT_ROUTER_ADDRESS), U256::from(100));
    assert_eq!(reserved(&mut ctx), U256::from(100));
    ctx.block.timestamp = U256::from(1900);
    let release = IVaultRouter::releaseReservationCall {
        id: ID,
        asset: ASSET,
        amount: U256::from(100),
        receiver: RECEIVER,
    }
    .abi_encode();
    ok_call(
        &mut ctx,
        CREDIS_FACTORY_ADDRESS,
        VAULT_ROUTER_ADDRESS,
        release.clone(),
    );
    assert_eq!(balance(&mut ctx, RECEIVER), U256::from(100));
    assert_eq!(balance(&mut ctx, VAULT_ROUTER_ADDRESS), U256::ZERO);
    assert_eq!(reserved(&mut ctx), U256::ZERO);
    let allowance = ok_call(
        &mut ctx,
        RECEIVER,
        ASSET,
        Token::allowanceCall {
            account: VAULT_ROUTER_ADDRESS,
            spender: RECEIVER,
        }
        .abi_encode(),
    );
    assert_eq!(
        Token::allowanceCall::abi_decode_returns(&allowance).unwrap(),
        U256::ZERO
    );
    assert!(!matches!(
        call(
            &mut ctx,
            CREDIS_FACTORY_ADDRESS,
            VAULT_ROUTER_ADDRESS,
            release
        )
        .status,
        SubCallStatus::Success
    ));
}

#[test]
fn failed_expiry_refund_retries_to_original_vault_and_never_delivers_late() {
    let mut ctx = setup();
    reserve(&mut ctx);
    ok_call(
        &mut ctx,
        RECEIVER,
        VAULT,
        Vault::setRejectDepositsCall { reject: true }.abi_encode(),
    );
    ctx.block.timestamp = U256::from(1901);
    let release = IVaultRouter::releaseReservationCall {
        id: ID,
        asset: ASSET,
        amount: U256::from(100),
        receiver: RECEIVER,
    }
    .abi_encode();
    assert!(!matches!(
        call(
            &mut ctx,
            CREDIS_FACTORY_ADDRESS,
            VAULT_ROUTER_ADDRESS,
            release
        )
        .status,
        SubCallStatus::Success
    ));
    ok_call(
        &mut ctx,
        RECEIVER,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::sweepExpiredPledgesCall { maxVisits: 256 }.abi_encode(),
    );
    assert_eq!(reserved(&mut ctx), U256::from(100));
    assert_eq!(balance(&mut ctx, VAULT_ROUTER_ADDRESS), U256::from(100));
    ok_call(
        &mut ctx,
        RECEIVER,
        VAULT,
        Vault::setRejectDepositsCall { reject: false }.abi_encode(),
    );
    ctx.block.timestamp = U256::from(2201);
    ok_call(
        &mut ctx,
        RECEIVER,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::sweepExpiredPledgesCall { maxVisits: 256 }.abi_encode(),
    );
    assert_eq!(reserved(&mut ctx), U256::ZERO);
    assert_eq!(balance(&mut ctx, VAULT), U256::from(1000));
    assert_eq!(balance(&mut ctx, OTHER_VAULT), U256::ZERO);
    assert_eq!(balance(&mut ctx, RECEIVER), U256::ZERO);
}

#[test]
fn taxed_token_cannot_create_an_underfunded_reservation() {
    let mut ctx = setup();
    ok_call(
        &mut ctx,
        RECEIVER,
        ASSET,
        Token::setFeeCall { enabled: true }.abi_encode(),
    );
    let result = call(
        &mut ctx,
        GRATIS_FACTORY_ADDRESS,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::reserveCall {
            id: ID,
            asset: ASSET,
            amount: U256::from(100),
            validUntil: 1900,
        }
        .abi_encode(),
    );
    assert!(!matches!(result.status, SubCallStatus::Success));
    assert_eq!(reserved(&mut ctx), U256::ZERO);
    assert_eq!(balance(&mut ctx, VAULT), U256::from(1000));
    assert_eq!(balance(&mut ctx, VAULT_ROUTER_ADDRESS), U256::ZERO);
}

const OWNER: Address = Address::new([0xA1; 20]);
const CCA: Address = Address::new([0xCC; 20]);

fn pledge_setup() -> (TestContext, Vec<u8>) {
    use outbe_gratis::enclave_client::test_enclave;
    use outbe_tee::pledgenote::*;
    test_enclave::install();
    let mut ctx = setup();
    let block = BlockContext::new(
        1,
        1000,
        outbe_primitives::chain::CHAIN_ID,
        RECEIVER,
        vec![RECEIVER],
    );
    let mut provider = DirectStorageProvider::new(&mut ctx.journaled_state.database, block);
    let request = StorageHandle::enter(&mut provider, |storage| {
        use outbe_tee::protocol::{GratisOp, ModifyAuth};
        let chain = B256::from(U256::from(storage.chain_id().unwrap()));
        let key = outbe_tee_enclave::gratis::derive_modify_key(&test_enclave::state_key(), OWNER)
            .unwrap();
        let amount = U256::from(100);
        let auth = ModifyAuth {
            mac: outbe_tee_enclave::gratis::modify_mac(
                &key,
                OWNER,
                GratisOp::Mint,
                amount,
                0,
                chain,
            ),
            op_nonce: 0,
        };
        outbe_gratis::api::mint(storage.clone(), OWNER, amount, auth).unwrap();
        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
        outbe_oracle::api::set_exchange_rate(
            storage.clone(),
            Address::ZERO,
            outbe_oracle::api::DAY_TYPE_PAIR,
            U256::from(2_000_000),
            1,
            1000,
        )
        .unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .reference_currencies
            .push(840)
            .unwrap();
        outbe_oracle::schema::OracleContract::new(storage.clone())
            .policy_rate
            .write(&840, U256::from(43_000))
            .unwrap();
        storage
            .increase_balance(CCA, U256::from(1_000_000_000_000_000u64))
            .unwrap();
        // Register the originating agent with real custody for its bond.
        let bond = outbe_ccaregistry::constants::BOND_REQUIREMENT;
        storage
            .increase_balance(outbe_primitives::addresses::CCA_REGISTRY_ADDRESS, bond)
            .unwrap();
        outbe_ccaregistry::runtime::bond(storage.clone(), CCA, bond, "test CCA".into()).unwrap();
        let quote = Quote {
            asset: ASSET,
            principal_minor: U256::from(100),
            max_gratis_minor: U256::from(50),
            reference_currency: 840,
        };
        let envelope =
            test_enclave::owner_envelope(&storage, OWNER, 1, OwnerAction::Create(quote.clone()));
        encode(&CreateRequest { quote, envelope }).unwrap()
    });
    provider.flush().unwrap();
    (ctx, request)
}

fn private_receipt(bytes: &[u8]) -> outbe_tee::pledgenote::Receipt {
    let key = outbe_tee_enclave::gratis::derive_view_key(
        &outbe_gratis::enclave_client::test_enclave::state_key(),
        OWNER,
    )
    .unwrap();
    outbe_tee::pledgenote::decrypt_receipt(&key, bytes).unwrap()
}

fn owner_query(ctx: &mut TestContext) -> outbe_tee::pledgenote::Receipt {
    use outbe_tee::pledgenote::*;
    let chain_id = B256::from(U256::from(outbe_primitives::chain::CHAIN_ID));
    let modify = outbe_tee_enclave::gratis::derive_modify_key(
        &outbe_gratis::enclave_client::test_enclave::state_key(),
        OWNER,
    )
    .unwrap();
    let action = OwnerAction::Query;
    let mac = owner_mac(&modify, chain_id, OWNER, 0, &action).unwrap();
    let envelope = encrypt_request(
        outbe_tee_enclave::crypto::x25519_public(&outbe_tee_enclave::dev::PLEDGE_OFFER_SECRET),
        &PrivateRequest::Owner {
            chain_id,
            account: OWNER,
            nonce: 0,
            action,
            mac,
        },
    )
    .unwrap();
    let data = outbe_gratis::precompile::IGratis::queryCall {
        encryptedRequest: envelope.into(),
    }
    .abi_encode();
    let bytes = ok_call(ctx, CCA, outbe_primitives::addresses::GRATIS_ADDRESS, data);
    private_receipt(
        &outbe_gratis::precompile::IGratis::queryCall::abi_decode_returns(&bytes).unwrap(),
    )
}

#[test]
fn private_note_delivers_reserved_tokens_and_repayment_restores_original_owner() {
    use outbe_credisfactory::precompile::ICredisFactory;
    use outbe_gratisfactory::precompile::IGratisFactory;
    use outbe_tee::pledgenote::*;
    let (mut ctx, request) = pledge_setup();
    let bytes = ok_call(
        &mut ctx,
        CCA,
        GRATIS_FACTORY_ADDRESS,
        IGratisFactory::createPledgeNoteCall {
            request: request.into(),
        }
        .abi_encode(),
    );
    let note =
        private_receipt(&IGratisFactory::createPledgeNoteCall::abi_decode_returns(&bytes).unwrap());
    assert_eq!(note.balance, U256::from(50));
    assert_eq!(reserved(&mut ctx), U256::from(100));
    assert_eq!(balance(&mut ctx, VAULT), U256::from(900));
    let chain_id = B256::from(U256::from(outbe_primitives::chain::CHAIN_ID));
    let envelope = encrypt_request(
        outbe_tee_enclave::crypto::x25519_public(&outbe_tee_enclave::dev::PLEDGE_OFFER_SECRET),
        &PrivateRequest::Use {
            chain_id,
            note_id: note.note_id,
            owner_sa: RECEIVER,
            authorization: use_mac(note.secret, chain_id, note.note_id, RECEIVER).unwrap(),
        },
    )
    .unwrap();
    let issue = ICredisFactory::issueCredisCall {
        ownerSA: RECEIVER,
        encryptedUseAuth: envelope.into(),
    }
    .abi_encode();
    // A failed delivery must restore both the private note and the public reservation.
    ok_call(
        &mut ctx,
        RECEIVER,
        ASSET,
        Token::setFeeCall { enabled: true }.abi_encode(),
    );
    let stake = outbe_primitives::units::checked_protocol_to_native(U256::from(50)).unwrap();
    let run_issue = |ctx: &mut TestContext| {
        use revm::context_interface::JournalTr;
        ctx.journaled_state.load_account(CCA).unwrap();
        sub_call::run(
            ctx,
            CCA,
            false,
            SpecId::PRAGUE,
            None,
            Arc::new(ExecutionScope::new()),
            SubCallInput {
                target: CREDIS_FACTORY_ADDRESS,
                value: stake,
                calldata: issue.clone().into(),
                gas_limit: 12_000_000,
                is_static: false,
            },
        )
        .unwrap()
    };
    assert!(!matches!(
        run_issue(&mut ctx).status,
        SubCallStatus::Success
    ));
    assert_eq!(reserved(&mut ctx), U256::from(100));
    assert_eq!(owner_query(&mut ctx).pledged, U256::from(50));
    ok_call(
        &mut ctx,
        RECEIVER,
        ASSET,
        Token::setFeeCall { enabled: false }.abi_encode(),
    );
    ctx.block.timestamp = U256::from(1900);
    let issued = run_issue(&mut ctx);
    assert!(
        matches!(issued.status, SubCallStatus::Success),
        "{:?}",
        issued.returndata
    );
    let result = ICredisFactory::issueCredisCall::abi_decode_returns(&issued.returndata).unwrap();
    assert_ne!(B256::from(result.credisId), note.note_id);
    assert_eq!(balance(&mut ctx, RECEIVER), U256::from(100));
    assert_eq!(balance(&mut ctx, VAULT), U256::from(900));
    assert_eq!(reserved(&mut ctx), U256::ZERO);
    assert!(!matches!(
        run_issue(&mut ctx).status,
        SubCallStatus::Success
    ));
    ok_call(
        &mut ctx,
        RECEIVER,
        ASSET,
        Token::approveCall {
            spender: CREDIS_FACTORY_ADDRESS,
            amount: U256::from(100),
        }
        .abi_encode(),
    );
    ok_call(
        &mut ctx,
        RECEIVER,
        CREDIS_FACTORY_ADDRESS,
        ICredisFactory::settleCall {
            positionId: result.credisId,
            amount: U256::from(40),
        }
        .abi_encode(),
    );
    assert_eq!(owner_query(&mut ctx).balance, U256::from(70));
    ok_call(
        &mut ctx,
        RECEIVER,
        CREDIS_FACTORY_ADDRESS,
        ICredisFactory::settleCall {
            positionId: result.credisId,
            amount: U256::from(60),
        }
        .abi_encode(),
    );
    assert_eq!(owner_query(&mut ctx).balance, U256::from(100));
    assert_eq!(owner_query(&mut ctx).pledged, U256::ZERO);
    assert_eq!(balance(&mut ctx, VAULT), U256::from(1000));
    outbe_gratis::enclave_client::test_enclave::uninstall();
}

#[test]
fn failed_quote_is_atomic_and_expired_cancel_restores_gratis_before_refund_retry() {
    use outbe_gratisfactory::precompile::IGratisFactory;
    use outbe_tee::pledgenote::*;
    let (mut ctx, request) = pledge_setup();
    let create = IGratisFactory::createPledgeNoteCall {
        request: request.into(),
    }
    .abi_encode();
    ok_call(
        &mut ctx,
        RECEIVER,
        ASSET,
        Token::setFeeCall { enabled: true }.abi_encode(),
    );
    assert!(!matches!(
        call(&mut ctx, CCA, GRATIS_FACTORY_ADDRESS, create.clone()).status,
        SubCallStatus::Success
    ));
    let restored = owner_query(&mut ctx);
    assert_eq!(restored.balance, U256::from(100));
    assert_eq!(restored.next_nonce, 1);
    assert_eq!(reserved(&mut ctx), U256::ZERO);
    ok_call(
        &mut ctx,
        RECEIVER,
        ASSET,
        Token::setFeeCall { enabled: false }.abi_encode(),
    );
    let created = ok_call(&mut ctx, CCA, GRATIS_FACTORY_ADDRESS, create);
    let note = private_receipt(
        &IGratisFactory::createPledgeNoteCall::abi_decode_returns(&created).unwrap(),
    );
    ok_call(
        &mut ctx,
        RECEIVER,
        VAULT,
        Vault::setRejectDepositsCall { reject: true }.abi_encode(),
    );
    ctx.block.timestamp = U256::from(1901);
    let chain_id = B256::from(U256::from(outbe_primitives::chain::CHAIN_ID));
    let modify = outbe_tee_enclave::gratis::derive_modify_key(
        &outbe_gratis::enclave_client::test_enclave::state_key(),
        OWNER,
    )
    .unwrap();
    let action = OwnerAction::Cancel {
        note_id: note.note_id,
    };
    let mac = owner_mac(&modify, chain_id, OWNER, 2, &action).unwrap();
    let envelope = encrypt_request(
        outbe_tee_enclave::crypto::x25519_public(&outbe_tee_enclave::dev::PLEDGE_OFFER_SECRET),
        &PrivateRequest::Owner {
            chain_id,
            account: OWNER,
            nonce: 2,
            action,
            mac,
        },
    )
    .unwrap();
    ok_call(
        &mut ctx,
        CCA,
        GRATIS_FACTORY_ADDRESS,
        IGratisFactory::cancelPledgeNoteCall {
            encryptedAuth: envelope.into(),
        }
        .abi_encode(),
    );
    assert_eq!(owner_query(&mut ctx).balance, U256::from(100));
    assert_eq!(owner_query(&mut ctx).pledged, U256::ZERO);
    assert_eq!(reserved(&mut ctx), U256::from(100));
    assert_eq!(balance(&mut ctx, VAULT_ROUTER_ADDRESS), U256::from(100));
    ok_call(
        &mut ctx,
        RECEIVER,
        VAULT,
        Vault::setRejectDepositsCall { reject: false }.abi_encode(),
    );
    ctx.block.timestamp = U256::from(2201);
    ok_call(
        &mut ctx,
        CCA,
        VAULT_ROUTER_ADDRESS,
        IVaultRouter::sweepExpiredPledgesCall { maxVisits: 256 }.abi_encode(),
    );
    assert_eq!(balance(&mut ctx, VAULT), U256::from(1000));
    assert_eq!(reserved(&mut ctx), U256::ZERO);
    outbe_gratis::enclave_client::test_enclave::uninstall();
}
