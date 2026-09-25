//! Real issuance with confidential note consumption and stateful ERC-20/vault calls.
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use outbe_compressed_entities::ExecutionScope;
use outbe_credisfactory::precompile::ICredisFactory;
use outbe_gratis::enclave_client::test_enclave;
use outbe_oracle::{api::AddressPair, schema::OracleContract};
use outbe_primitives::{
    addresses::{CCA_REGISTRY_ADDRESS, CREDIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS},
    block::BlockContext,
    chain::CHAIN_ID,
    storage::{direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallStatus},
    time::{previous_date_key, timestamp_to_date_key},
};
use outbe_tee::protocol::{GratisOp, ModifyAuth, PledgeTerms};
use outbe_tee_enclave::gratis::{derive_modify_key, modify_mac, pledge_secret, spend_auth_mac};
use outbe_vaultrouter::{api::IVaultRouter, LiquidityReservation, VaultRouterContract};
use revm::{
    context_interface::JournalTr,
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Context,
};
use std::sync::Arc;

sol! {
    interface IFixture {
        function mint(address account, uint256 amount) external;
        function configure(uint256 mode) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }
}
const OWNER: Address = Address::new([0x11; 20]);
const CCA: Address = Address::new([0x22; 20]);
const ASSET: Address = Address::new([0x33; 20]);
const ACCOUNT: Address = Address::new([0x44; 20]);
const VAULT: Address = Address::new([0x55; 20]);
const NOW: u64 = 1_700_000_000;

#[test]
fn issuance_pays_cca_preserves_account_stables_and_rolls_back_failed_payouts() {
    // 0: success; 1: ERC20 false; 2: ERC20 revert; 3: excess redeposit revert;
    // 4: wrong contribution; 5: invalid spend authorization; 6/7: changed asset metadata.
    for failure in 0..8 {
        test_enclave::install();
        let key = derive_modify_key(&test_enclave::state_key(), OWNER).unwrap();
        let mut db = CacheDB::new(EmptyDB::default());
        let code = Bytecode::new_raw(Bytes::from(
            alloy_primitives::hex::decode(include_str!("fixtures/CredisIssuance.hex").trim())
                .unwrap(),
        ));
        for addr in [ASSET, VAULT] {
            db.insert_account_info(
                addr,
                AccountInfo {
                    code_hash: code.hash_slow(),
                    code: Some(code.clone()),
                    ..Default::default()
                },
            );
        }
        let account_code = Bytecode::new_raw(Bytes::from_static(&[0x00]));
        db.insert_account_info(
            ACCOUNT,
            AccountInfo {
                code_hash: account_code.hash_slow(),
                code: Some(account_code),
                ..Default::default()
            },
        );
        let stake = U256::from(10).pow(U256::from(18));
        db.insert_account_info(
            CCA,
            AccountInfo {
                balance: stake * U256::from(2),
                ..Default::default()
            },
        );
        let mut provider = DirectStorageProvider::new(
            &mut db,
            BlockContext::new(1, NOW, CHAIN_ID, OWNER, vec![OWNER]),
        );
        let note = StorageHandle::enter(&mut provider, |storage| {
            storage
                .increase_balance(
                    CCA_REGISTRY_ADDRESS,
                    outbe_ccaregistry::constants::BOND_REQUIREMENT,
                )
                .unwrap();
            outbe_ccaregistry::runtime::bond(
                storage.clone(),
                CCA,
                outbe_ccaregistry::constants::BOND_REQUIREMENT,
                "CCA".into(),
            )
            .unwrap();
            let router = VaultRouterContract::new(storage.clone());
            router
                .liquidity_targets
                .insert(CREDIS_FACTORY_ADDRESS)
                .unwrap();
            router
                .liquidity_target_types
                .write(
                    &CREDIS_FACTORY_ADDRESS,
                    IVaultRouter::StablesTarget::Credis as u8,
                )
                .unwrap();
            // Reserve before pledging; the held assets are funded below through real ERC20 calls.
            router
                .reservations
                .create(&LiquidityReservation {
                    id: U256::ONE,
                    asset: ASSET,
                    amount: U256::from(3_000_000),
                    smart_account: ACCOUNT,
                    cca: CCA,
                    vault: VAULT,
                    expires_at: NOW + 900,
                })
                .unwrap();
            let price = U256::from(2_000_000);
            outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(840))
                .unwrap();
            outbe_oracle::api::set_exchange_rate(
                storage.clone(),
                Address::ZERO,
                AddressPair::new_coen_to(840),
                price,
                1,
                NOW,
            )
            .unwrap();
            let oracle = OracleContract::new(storage.clone());
            oracle.reference_currencies.push(840).unwrap();
            oracle.policy_rate.write(&840, U256::from(43_000)).unwrap();
            let day = previous_date_key(timestamp_to_date_key(NOW));
            let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)
                .unwrap()
                .unwrap();
            oracle
                .utc_day_vwap_value
                .get_nested(&day)
                .write(&index, price)
                .unwrap();
            oracle.utc_day_vwap_last_finalized.write(day).unwrap();
            let auth = |op, amount, op_nonce| ModifyAuth {
                mac: modify_mac(
                    &key,
                    OWNER,
                    op,
                    amount,
                    op_nonce,
                    B256::from(U256::from(CHAIN_ID)),
                ),
                op_nonce,
            };
            let gratis = U256::from(1_000_000);
            outbe_gratis::api::mint(
                storage.clone(),
                OWNER,
                gratis,
                auth(GratisOp::Mint, gratis, 0),
            )
            .unwrap();
            outbe_gratis::api::pledge(
                storage,
                OWNER,
                price,
                PledgeTerms {
                    stables_amount: price,
                    gratis_amount: gratis,
                    asset: ASSET,
                    entry_price: price,
                    issuance_currency: 840,
                    asset_decimals: 6,
                    valuation_price: U256::from(2) * outbe_primitives::units::SCALE_1E18,
                },
                auth(GratisOp::Pledge, price, 1),
            )
            .unwrap()
        });
        provider.flush().unwrap();
        let mut ctx = Context::mainnet()
            .with_db(db)
            .modify_cfg_chained(|cfg| cfg.chain_id = CHAIN_ID)
            .modify_block_chained(|block| block.timestamp = U256::from(NOW));
        let scope = Arc::new(ExecutionScope::new());
        macro_rules! call {
            ($caller:expr, $target:expr, $value:expr, $call:expr) => {{
                ctx.journaled_state.load_account($caller).unwrap();
                outbe_evm::sub_call::run(
                    &mut ctx,
                    $caller,
                    false,
                    SpecId::PRAGUE,
                    None,
                    scope.clone(),
                    SubCallInput {
                        target: $target,
                        value: $value,
                        calldata: $call.abi_encode().into(),
                        gas_limit: 10_000_000,
                        is_static: false,
                    },
                )
                .unwrap()
            }};
        }
        for (account, amount) in [(ACCOUNT, 2_000_000), (VAULT_ROUTER_ADDRESS, 3_000_000)] {
            assert!(matches!(
                call!(
                    OWNER,
                    ASSET,
                    U256::ZERO,
                    IFixture::mintCall {
                        account,
                        amount: U256::from(amount)
                    }
                )
                .status,
                SubCallStatus::Success
            ));
        }
        assert!(matches!(
            call!(
                VAULT_ROUTER_ADDRESS,
                ASSET,
                U256::ZERO,
                IFixture::approveCall {
                    spender: VAULT,
                    amount: U256::MAX
                }
            )
            .status,
            SubCallStatus::Success
        ));
        if (1..=3).contains(&failure) || failure >= 6 {
            let target = if failure == 3 { VAULT } else { ASSET };
            assert!(matches!(
                call!(
                    OWNER,
                    target,
                    U256::ZERO,
                    IFixture::configureCall {
                        mode: U256::from(failure)
                    }
                )
                .status,
                SubCallStatus::Success
            ));
        }
        let spend = spend_auth_mac(&pledge_secret(&key, note), ACCOUNT);
        let issue = ICredisFactory::issueCredisCall {
            smartAccount: ACCOUNT,
            pledgeNote: note,
            spendAuth: if failure == 5 {
                B256::ZERO
            } else {
                B256::from(spend)
            },
            referenceCurrency: 840,
            reservationId: U256::ONE,
        };
        let out = call!(
            CCA,
            CREDIS_FACTORY_ADDRESS,
            if failure == 4 {
                stake - U256::ONE
            } else {
                stake
            },
            issue.clone()
        );
        assert_eq!(
            matches!(out.status, SubCallStatus::Success),
            failure == 0,
            "failure mode {failure}: {:?} {}",
            out.status,
            String::from_utf8_lossy(&out.returndata)
        );
        assert_eq!(
            ctx.journaled_state
                .load_account(ACCOUNT)
                .unwrap()
                .info
                .balance,
            if failure == 0 { stake } else { U256::ZERO }
        );
        assert_eq!(
            ctx.journaled_state.load_account(CCA).unwrap().info.balance,
            if failure == 0 {
                stake
            } else {
                stake * U256::from(2)
            }
        );
        for (account, expected) in [
            (ACCOUNT, 2_000_000),
            (CCA, if failure == 0 { 2_000_000 } else { 0 }),
            (VAULT, if failure == 0 { 1_000_000 } else { 0 }),
            (
                VAULT_ROUTER_ADDRESS,
                if failure == 0 { 0 } else { 3_000_000 },
            ),
        ] {
            let balance = call!(
                OWNER,
                ASSET,
                U256::ZERO,
                IFixture::balanceOfCall { account }
            );
            assert_eq!(
                IFixture::balanceOfCall::abi_decode_returns(&balance.returndata).unwrap(),
                U256::from(expected)
            );
        }
        let reservation = call!(
            OWNER,
            VAULT_ROUTER_ADDRESS,
            U256::ZERO,
            IVaultRouter::reservationOfCall { id: U256::ONE }
        );
        let held =
            IVaultRouter::reservationOfCall::abi_decode_returns(&reservation.returndata).unwrap();
        assert_eq!(
            held.amount,
            if failure == 0 {
                U256::ZERO
            } else {
                U256::from(3_000_000)
            }
        );
        let payouts: Vec<_> = ctx
            .journaled_state
            .logs()
            .iter()
            .filter_map(|log| IVaultRouter::ReservationReleased::decode_log_data(&log.data).ok())
            .collect();
        assert_eq!(payouts.len(), usize::from(failure == 0));
        if failure == 0 {
            assert_eq!(payouts[0].receiver, CCA);
            assert_eq!(payouts[0].amount, U256::from(2_000_000));
        }
        // Reset counterparties and retry with the original note and exact contribution.
        for target in [ASSET, VAULT] {
            call!(
                OWNER,
                target,
                U256::ZERO,
                IFixture::configureCall { mode: U256::ZERO }
            );
        }
        let retry = call!(
            CCA,
            CREDIS_FACTORY_ADDRESS,
            stake,
            ICredisFactory::issueCredisCall {
                spendAuth: B256::from(spend),
                ..issue
            }
        );
        assert_eq!(
            matches!(retry.status, SubCallStatus::Success),
            failure != 0,
            "failed issuance must restore the note and position; success must prevent replay: {:?}",
            retry.status
        );
        test_enclave::uninstall();
    }
}
