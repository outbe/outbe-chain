//! Real issuance against a direct Gratis pledge with stateful ERC-20/vault calls.
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use outbe_credisfactory::precompile::ICredisFactory;
use outbe_evm::test_support::{install_code, runtime_code, seed_storage, EvmWorld};
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratis::precompile::IGratis;
use outbe_gratisfactory::precompile::IGratisFactory;
use outbe_oracle::{
    schema::OracleContract,
    test_support::{publish_day_type_quote, record_finalized_day_vwap},
};
use outbe_primitives::{
    addresses::{
        CCA_REGISTRY_ADDRESS, CREDIS_FACTORY_ADDRESS, GRATIS_ADDRESS, GRATIS_FACTORY_ADDRESS,
        VAULT_ROUTER_ADDRESS,
    },
    chain::CHAIN_ID,
    storage::{StorageHandle, SubCallInput, SubCallStatus},
    time::{previous_date_key, timestamp_to_date_key},
};
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{
    decrypt_balance, decrypt_pledged, derive_modify_key, derive_view_key, modify_mac,
};
use outbe_vaultrouter::{
    api::IVaultRouter,
    test_support::{register_source, register_target},
    LiquidityReservation, VaultRouterContract,
};
use revm::{
    context_interface::JournalTr,
    database::{CacheDB, EmptyDB},
    state::AccountInfo,
};

sol! {
    interface IFixture {
        function mint(address account, uint256 amount) external;
        function configure(uint256 mode) external;
        function setPosition(uint256 id) external;
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

macro_rules! call {
    ($evm:expr, $caller:expr, $target:expr, $value:expr, $call:expr) => {{
        $evm.ctx.journaled_state.load_account($caller)?;
        $evm.run(
            $caller,
            SubCallInput {
                target: $target,
                value: $value,
                calldata: $call.abi_encode().into(),
                gas_limit: 10_000_000,
                is_static: false,
            },
        )?
    }};
}

struct PaymentObservation {
    credis_id: U256,
    credis_call: outbe_credis::precompile::ICredis::getCredisCall,
    before: Bytes,
    pledged: Bytes,
}
#[test]
fn issuance_pays_cca_preserves_account_stables_and_rolls_back_failed_payouts() {
    // Preserve the original success, payout/authorization/metadata failure and expiry cases.
    for failure in [0, 1, 2, 4, 5, 6, 7, 8] {
        test_enclave::install();
        let key = derive_modify_key(&test_enclave::state_key(), OWNER).unwrap();
        let (mut db, stake) = issuance_database().expect("issuance database fixture succeeds");
        seed_storage(&mut db, NOW, OWNER, |storage| {
            seed_issuance_liquidity(storage.clone())
                .expect("seed issuance liquidity fixture succeeds");
            seed_issuance_oracle(storage.clone()).expect("seed issuance oracle fixture succeeds");
            mint_source_gratis(storage, &key).expect("mint source gratis fixture succeeds");
        });
        let mut evm = EvmWorld::new(db).at(NOW);
        prepare_issuance_counterparties(&mut evm, failure)
            .expect("prepare issuance counterparties fixture succeeds");
        if failure != 5 {
            pledge_reservation(&mut evm, &key).expect("pledge reservation fixture succeeds");
        }
        let (issue, out) =
            issue_cca_payout(&mut evm, failure, stake).expect("issue cca payout fixture succeeds");
        assert_issuance_state(&mut evm, failure, stake)
            .expect("assert issuance state fixture succeeds");
        retry_issuance_and_cancel_expired(&mut evm, failure, stake, issue, &key)
            .expect("retry issuance and cancel expired fixture succeeds");
        if failure == 0 {
            settle_successful_issuance(&mut evm, &out)
                .expect("settle successful issuance fixture succeeds");
        }
        test_enclave::uninstall();
    }
}

fn issuance_database() -> eyre::Result<(CacheDB<EmptyDB>, U256)> {
    let mut db = CacheDB::new(EmptyDB::default());
    let code = runtime_code(include_str!(
        "../../../../testing/fixtures/evm/CredisIssuance.hex"
    ));
    for addr in [ASSET, VAULT] {
        install_code(&mut db, addr, &code);
    }
    let stake = U256::from(10).pow(U256::from(18));
    db.insert_account_info(
        CCA,
        AccountInfo {
            balance: stake * U256::from(2),
            ..Default::default()
        },
    );

    Ok((db, stake))
}

fn seed_issuance_liquidity(storage: StorageHandle<'_>) -> eyre::Result<()> {
    storage.increase_balance(
        CCA_REGISTRY_ADDRESS,
        outbe_ccaregistry::constants::BOND_REQUIREMENT,
    )?;
    outbe_ccaregistry::runtime::bond(
        storage.clone(),
        CCA,
        outbe_ccaregistry::constants::BOND_REQUIREMENT,
        "CCA".into(),
    )?;
    let router = VaultRouterContract::new(storage.clone());
    router.asset_vault_set(ASSET).insert(VAULT)?;
    register_source(
        &storage,
        CREDIS_FACTORY_ADDRESS,
        IVaultRouter::StablesSource::CredisCostAmount,
    )?;
    register_target(
        &storage,
        CREDIS_FACTORY_ADDRESS,
        IVaultRouter::StablesTarget::Credis,
    )?;
    // Reserve before pledging. The code below funds the held assets through real ERC20 calls.
    router.reservations.create(&LiquidityReservation {
        id: U256::ONE,
        asset: ASSET,
        amount: U256::from(2_000_000),
        credis_owner: ACCOUNT,
        cca: CCA,
        vault: VAULT,
        expires_at: NOW + 900,
        gratis_minor: U256::from(1_000_000),
        snapshot_id: U256::from(17),
        entry_price_minor: U256::from(2_000_000),
        valuation_price_minor: U256::from(2_000_000),
        policy_rate: U256::from(43_000),
        issuance_currency: 840,
        asset_decimals: 6,
        reference_currency: 840,
        call_anchor_price_minor: U256::from(2_000_000),
        source: OWNER,
    })?;

    Ok(())
}

fn seed_issuance_oracle(storage: StorageHandle<'_>) -> eyre::Result<()> {
    let price = U256::from(2_000_000);
    publish_day_type_quote(&storage, price, 1, NOW)?;
    OracleContract::new(storage.clone())
        .policy_rate
        .write(&840, U256::from(43_000))?;
    let day = previous_date_key(timestamp_to_date_key(NOW));
    record_finalized_day_vwap(&storage, 840, day, price)?;

    Ok(())
}

const GRATIS: u64 = 1_000_000;

fn source_auth(key: &[u8; 32], op: GratisOp, op_nonce: u64) -> ModifyAuth {
    ModifyAuth {
        mac: modify_mac(
            key,
            &outbe_tee_enclave::gratis::ModifyOperation {
                account: OWNER,
                op,
                amount: U256::from(GRATIS),
                op_nonce,
                chain_id: B256::from(U256::from(CHAIN_ID)),
            },
        ),
        op_nonce,
    }
}

fn mint_source_gratis(storage: StorageHandle<'_>, key: &[u8; 32]) -> eyre::Result<()> {
    outbe_gratis::api::mint(
        storage,
        OWNER,
        U256::from(GRATIS),
        source_auth(key, GratisOp::Mint, 0),
    )?;

    Ok(())
}

fn pledge_reservation(evm: &mut EvmWorld, key: &[u8; 32]) -> eyre::Result<()> {
    let auth = source_auth(key, GratisOp::Pledge, 1);
    let pledge = IGratisFactory::pledgeGratisCall {
        reservationId: U256::ONE,
        auth: IGratisFactory::ModifyAuth {
            mac: auth.mac.into(),
            opNonce: auth.op_nonce,
        },
    };
    assert!(!matches!(
        call!(evm, CCA, GRATIS_FACTORY_ADDRESS, U256::ZERO, pledge.clone()).status,
        SubCallStatus::Success
    ));
    let out = call!(evm, OWNER, GRATIS_FACTORY_ADDRESS, U256::ZERO, pledge);
    assert!(
        matches!(out.status, SubCallStatus::Success),
        "{}",
        String::from_utf8_lossy(&out.returndata)
    );

    Ok(())
}

fn pledged_blob(evm: &mut EvmWorld) -> eyre::Result<Bytes> {
    Ok(call!(
        evm,
        OWNER,
        GRATIS_ADDRESS,
        U256::ZERO,
        IGratis::pledgedOfCall { account: OWNER }
    )
    .returndata)
}

/// The source's decrypted liquid and pledged Gratis.
fn source_gratis(evm: &mut EvmWorld) -> eyre::Result<(U256, U256)> {
    let view_key = derive_view_key(&test_enclave::state_key(), OWNER)?;
    let liquid = call!(
        evm,
        OWNER,
        GRATIS_ADDRESS,
        U256::ZERO,
        IGratis::balanceOfCall { account: OWNER }
    );
    let liquid = IGratis::balanceOfCall::abi_decode_returns(&liquid.returndata)?;
    let pledged = IGratis::pledgedOfCall::abi_decode_returns(&pledged_blob(evm)?)?;
    Ok((
        decrypt_balance(&view_key, OWNER, &liquid)?,
        if pledged.is_empty() {
            U256::ZERO
        } else {
            decrypt_pledged(&view_key, OWNER, &pledged)?
        },
    ))
}

fn prepare_issuance_counterparties(evm: &mut EvmWorld, failure: u64) -> eyre::Result<()> {
    for (account, amount) in [(ACCOUNT, 2_000_000), (VAULT_ROUTER_ADDRESS, 2_000_000)] {
        assert!(matches!(
            call!(
                evm,
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
            evm,
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
                evm,
                OWNER,
                target,
                U256::ZERO,
                IFixture::configureCall {
                    mode: U256::from(if failure == 8 { 1 } else { failure })
                }
            )
            .status,
            SubCallStatus::Success
        ));
    }

    Ok(())
}

fn issue_cca_payout(
    evm: &mut EvmWorld,
    failure: u64,
    stake: U256,
) -> eyre::Result<(
    ICredisFactory::issueCredisCall,
    outbe_primitives::storage::SubCallOutput,
)> {
    let issue = ICredisFactory::issueCredisCall {
        reservationId: U256::ONE,
    };
    let out = call!(
        evm,
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

    Ok((issue, out))
}

fn assert_issuance_state(evm: &mut EvmWorld, failure: u64, stake: U256) -> eyre::Result<()> {
    assert_issuance_native_balances(evm, failure, stake)?;
    for (account, expected) in [
        (ACCOUNT, 2_000_000),
        (CCA, if failure == 0 { 2_000_000 } else { 0 }),
        (VAULT, 0),
        (
            VAULT_ROUTER_ADDRESS,
            if failure == 0 { 0 } else { 2_000_000 },
        ),
    ] {
        let balance = call!(
            evm,
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
        evm,
        OWNER,
        VAULT_ROUTER_ADDRESS,
        U256::ZERO,
        IVaultRouter::reservationOfCall { id: U256::ONE }
    );
    let held = IVaultRouter::reservationOfCall::abi_decode_returns(&reservation.returndata)?;
    assert_eq!(
        held.amount,
        if failure == 0 {
            U256::ZERO
        } else {
            U256::from(2_000_000)
        }
    );
    assert_issuance_payout_events(evm, failure)?;

    Ok(())
}

fn retry_issuance_and_cancel_expired(
    evm: &mut EvmWorld,
    failure: u64,
    stake: U256,
    issue: ICredisFactory::issueCredisCall,
    key: &[u8; 32],
) -> eyre::Result<()> {
    // Reset counterparties and retry with the same pledge and exact contribution.
    for target in [ASSET, VAULT] {
        call!(
            evm,
            OWNER,
            target,
            U256::ZERO,
            IFixture::configureCall { mode: U256::ZERO }
        );
    }
    if failure == 5 {
        pledge_reservation(evm, key)?;
    }
    evm.ctx.block.timestamp = U256::from(NOW + if failure == 8 { 901 } else { 900 });
    let retry = call!(evm, CCA, CREDIS_FACTORY_ADDRESS, stake, issue);
    assert_eq!(
        matches!(retry.status, SubCallStatus::Success),
        failure != 0 && failure != 8,
        "failed issuance must keep the pledge; success must prevent replay: {:?}",
        retry.status
    );
    if failure == 8 {
        assert!(String::from_utf8_lossy(&retry.returndata).contains("expired"));
        assert_eq!(source_gratis(evm)?, (U256::ZERO, U256::from(GRATIS)));
        let cancel = IGratisFactory::cancelPledgeCall {
            reservationId: U256::ONE,
        };
        assert!(!matches!(
            call!(evm, CCA, GRATIS_FACTORY_ADDRESS, U256::ZERO, cancel.clone()).status,
            SubCallStatus::Success
        ));
        assert!(matches!(
            call!(
                evm,
                OWNER,
                GRATIS_FACTORY_ADDRESS,
                U256::ZERO,
                cancel.clone()
            )
            .status,
            SubCallStatus::Success
        ));
        let cancelled: Vec<_> = evm
            .ctx
            .journaled_state
            .logs()
            .iter()
            .filter_map(|log| IGratisFactory::PledgeCancelled::decode_log_data(&log.data).ok())
            .collect();
        assert_eq!(cancelled.len(), 1);
        assert_eq!(
            (cancelled[0].source, cancelled[0].gratisMinor),
            (OWNER, U256::from(GRATIS))
        );
        assert_eq!(source_gratis(evm)?, (U256::from(GRATIS), U256::ZERO));
        assert!(!matches!(
            call!(evm, OWNER, GRATIS_FACTORY_ADDRESS, U256::ZERO, cancel).status,
            SubCallStatus::Success
        ));
    }

    Ok(())
}

fn assert_payment_rollback(
    evm: &mut EvmWorld,
    mode: u64,
    observation: &PaymentObservation,
) -> eyre::Result<()> {
    let target = if mode == 3 { VAULT } else { ASSET };
    call!(
        evm,
        OWNER,
        target,
        U256::ZERO,
        IFixture::configureCall {
            mode: U256::from(mode)
        }
    );
    let logs = evm.ctx.journaled_state.logs().len();
    let payment = call!(
        evm,
        CCA,
        CREDIS_FACTORY_ADDRESS,
        U256::ZERO,
        ICredisFactory::settleCredisCall {
            credisId: observation.credis_id,
            amountMinor: U256::from(1_000_000)
        }
    );
    assert!(
        !matches!(payment.status, SubCallStatus::Success),
        "payment mode {mode} must roll back"
    );
    if mode == 9 {
        assert!(
            String::from_utf8_lossy(&payment.returndata)
                .contains("outbe precompile reentrancy denied"),
            "{:?}",
            payment.returndata
        );
    }
    assert_eq!(evm.ctx.journaled_state.logs().len(), logs);
    assert_payment_credis_and_pledge_unchanged(evm, observation)?;
    assert_payment_token_balances(evm)?;
    call!(
        evm,
        OWNER,
        target,
        U256::ZERO,
        IFixture::configureCall { mode: U256::ZERO }
    );

    Ok(())
}

fn settle_successful_issuance(
    evm: &mut EvmWorld,
    out: &outbe_primitives::storage::SubCallOutput,
) -> eyre::Result<()> {
    use outbe_credis::precompile::ICredis;
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    let credis_id = ICredisFactory::issueCredisCall::abi_decode_returns(&out.returndata)?.credisId;
    call!(
        evm,
        OWNER,
        ASSET,
        U256::ZERO,
        IFixture::setPositionCall { id: credis_id }
    );
    call!(
        evm,
        CCA,
        ASSET,
        U256::ZERO,
        IFixture::approveCall {
            spender: CREDIS_FACTORY_ADDRESS,
            amount: U256::MAX
        }
    );
    let credis_call = ICredis::getCredisCall {
        credisId: credis_id,
    };
    let before = call!(evm, OWNER, CREDIS_ADDRESS, U256::ZERO, credis_call.clone()).returndata;
    let pledged = pledged_blob(evm)?;
    let payment = PaymentObservation {
        credis_id,
        credis_call,
        before,
        pledged,
    };
    for mode in [1, 2, 3, 9, 10] {
        assert_payment_rollback(evm, mode, &payment)?;
    }
    let paid = call!(
        evm,
        CCA,
        CREDIS_FACTORY_ADDRESS,
        U256::ZERO,
        ICredisFactory::settleCredisCall {
            credisId: credis_id,
            amountMinor: U256::from(1_000_000)
        }
    );
    assert!(
        matches!(paid.status, SubCallStatus::Success),
        "{:?}",
        paid.returndata
    );
    let half = U256::from(GRATIS / 2);
    assert_eq!(source_gratis(evm)?, (half, half));

    Ok(())
}

fn assert_issuance_native_balances(
    evm: &mut EvmWorld,
    failure: u64,
    stake: U256,
) -> eyre::Result<()> {
    assert_eq!(
        evm.ctx
            .journaled_state
            .load_account(ACCOUNT)
            .unwrap()
            .info
            .balance,
        if failure == 0 { stake } else { U256::ZERO }
    );
    assert_eq!(
        evm.ctx
            .journaled_state
            .load_account(CCA)
            .unwrap()
            .info
            .balance,
        if failure == 0 {
            stake
        } else {
            stake * U256::from(2)
        }
    );

    Ok(())
}

fn assert_issuance_payout_events(evm: &EvmWorld, failure: u64) -> eyre::Result<()> {
    let payouts: Vec<_> = evm
        .ctx
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

    Ok(())
}

fn assert_payment_credis_and_pledge_unchanged(
    evm: &mut EvmWorld,
    observation: &PaymentObservation,
) -> eyre::Result<()> {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    assert_eq!(
        call!(
            evm,
            OWNER,
            CREDIS_ADDRESS,
            U256::ZERO,
            observation.credis_call.clone()
        )
        .returndata,
        observation.before
    );
    assert_eq!(pledged_blob(evm)?, observation.pledged);

    Ok(())
}

fn assert_payment_token_balances(evm: &mut EvmWorld) -> eyre::Result<()> {
    for (account, expected) in [
        (CCA, 2_000_000),
        (VAULT, 0),
        (VAULT_ROUTER_ADDRESS, 0),
        (CREDIS_FACTORY_ADDRESS, 0),
    ] {
        let balance = call!(
            evm,
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

    Ok(())
}
