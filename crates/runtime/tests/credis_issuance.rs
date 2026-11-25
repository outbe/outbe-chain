//! Real issue by the cash account with stateful ERC-20/vault calls and a sealed pledge.
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use evm2::evm::{CacheDB, EmptyDB};
use outbe_core_common::addresses::WCOEN_ADDRESS;
use outbe_credisfactory::precompile::ICredisFactory;
use outbe_evm::test_support::{install_code, runtime_code, seed_storage, EvmWorld};
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratis::precompile::IGratis;
use outbe_gratis::{
    decrypt_balance, decrypt_pledged, derive_account_keys, derive_view_key, modify_mac,
};
use outbe_gratis::{GratisOp, ModifyAuth};
use outbe_oracle::{
    schema::OracleContract,
    test_support::{publish_day_type_rate, record_finalized_day_vwap},
};
use outbe_primitives::{
    chain::CHAIN_ID,
    storage::{StorageHandle, SubCallInput, SubCallStatus},
};
use outbe_system_common::addresses::{
    CCA_REGISTRY_ADDRESS, CREDIS_FACTORY_ADDRESS, GRATIS_ADDRESS, VAULT_ROUTER_ADDRESS,
};
use outbe_system_common::time::{previous_date_key, timestamp_to_date_key};
use outbe_vaultrouter::{
    precompile::IVaultRouter,
    test_support::{register_source, register_target},
    CredisReservation, VaultRouterContract,
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
const OMNIBUS: Address = Address::new([0x66; 20]);
const AMOUNT: u64 = 2_000_000;
const NOW: u64 = 1_700_000_000;

macro_rules! call {
    ($evm:expr, $caller:expr, $target:expr, $value:expr, $call:expr) => {{
        $evm.evm.state_mut().account(&$caller)?;
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

/// Each failure mode of the issue: 0 succeeds; 1 and 2 fail a token transfer; 5 brings a pledge
/// sealed for another reservation; 6 and 7 change the asset metadata; 8 issues after expiry.
#[test]
fn issue_reverts_as_a_whole_at_each_fault() {
    for failure in [0, 1, 2, 5, 6, 7, 8] {
        test_enclave::install();
        let key = derive_account_keys(&test_enclave::state_key(), OWNER).unwrap();
        let mut db = issuance_database();
        seed_storage(&mut db, NOW, OWNER, |storage| {
            seed_issuance_liquidity(storage.clone())
                .expect("seed issuance liquidity fixture succeeds");
            seed_issuance_oracle(storage.clone()).expect("seed issuance oracle fixture succeeds");
            mint_source_gratis(storage, &key).expect("mint source gratis fixture succeeds");
        });
        let mut evm =
            EvmWorld::new(db, std::sync::Arc::new(outbe_runtime::OutbeRuntime::new())).at(NOW);
        prepare_issuance_counterparties(&mut evm, failure)
            .expect("prepare issuance counterparties fixture succeeds");
        if failure == 8 {
            let mut block = *evm.evm.block();
            block.timestamp = U256::from(NOW + 901);
            evm.evm.set_block(block);
        }
        let pledge = sealed(if failure == 5 { 2 } else { 1 });
        let out = issue(&mut evm, &pledge, failure == 0).expect("issue fixture succeeds");
        assert_issuance_state(&mut evm, failure == 0).expect("issue state fixture succeeds");
        reset_counterparties(&mut evm).expect("reset counterparties fixture succeeds");
        if failure == 0 {
            // The reservation is gone, so the same pledge cannot issue twice.
            issue(&mut evm, &pledge, false).expect("replayed issue fixture succeeds");
            settle_successful_issuance(&mut evm, &out)
                .expect("settle successful issuance fixture succeeds");
        } else if failure != 8 {
            // A failed issue moved nothing, so the same reservation issues on retry.
            issue(&mut evm, &sealed(1), true).expect("retried issue fixture succeeds");
            assert_issuance_state(&mut evm, true).expect("retried issue state fixture succeeds");
        }
        test_enclave::uninstall();
    }
}

fn issuance_database() -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    let code = runtime_code(include_str!(
        "../../../testing/fixtures/evm/CredisIssuance.hex"
    ));
    for addr in [ASSET, VAULT, WCOEN_ADDRESS] {
        install_code(&mut db, addr, &code);
    }
    db
}

fn wcoen() -> U256 {
    outbe_system_common::units::checked_protocol_to_native(U256::from(GRATIS))
        .expect("WCOEN of the fixture fits")
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
        IVaultRouter::StablecoinSource::CredisCostAmount,
    )?;
    register_target(
        &storage,
        CREDIS_FACTORY_ADDRESS,
        IVaultRouter::StablecoinTarget::Credis,
    )?;
    // The escrow and the WCOEN of the reservation are funded through real ERC20 calls below.
    let reservation_day = timestamp_to_date_key(NOW);
    let call_terms = outbe_credis::config::read(&storage)?;
    router.reservations.create(&CredisReservation {
        id: U256::ONE,
        asset: ASSET,
        amount_minor: U256::from(AMOUNT),
        credis_owner: ACCOUNT,
        cca: CCA,
        vault: VAULT,
        expires_at: NOW + 900,
        gratis_minor: U256::from(GRATIS),
        snapshot_id: U256::from(17),
        entry_price_minor: U256::from(2_000_000),
        policy_rate: U256::from(43_000),
        issuance_currency: 840,
        asset_decimals: 6,
        reference_currency: 840,
        series_id: outbe_core_common::series_id::SeriesId::for_pair(reservation_day, 840, 840)
            .expect("series id"),
        sub_series: outbe_credis::CredisContract::sub_series_of(NOW),
        call_start_day: outbe_system_common::time::first_full_day(NOW).expect("date step"),
        call_rate: call_terms.call_rate,
        call_window_seconds: call_terms.call_window_seconds,
        call_threshold_seconds: call_terms.call_threshold_seconds,
        call_notice_period_seconds: call_terms.call_notice_period_seconds,
        wcoen_minor: wcoen(),
        cca_receiver: CCA,
        omnibus: OMNIBUS,
    })?;

    Ok(())
}

fn seed_issuance_oracle(storage: StorageHandle<'_>) -> eyre::Result<()> {
    let price = U256::from(2_000_000);
    publish_day_type_rate(&storage, price, 1, NOW)?;
    let oracle = OracleContract::new(storage.clone());
    oracle.policy_rate.write(&840, U256::from(43_000))?;
    // A publication time marks the rate as closed.
    oracle.policy_rate_published_at.write(&840, 1)?;
    let day = previous_date_key(timestamp_to_date_key(NOW)).expect("date step");
    record_finalized_day_vwap(&storage, 840, day, price)?;

    Ok(())
}

const GRATIS: u64 = 1_000_000;

fn source_auth(
    key: &outbe_tee::resident::AccountKeysHandle,
    op: GratisOp,
    op_nonce: u64,
) -> ModifyAuth {
    ModifyAuth {
        mac: modify_mac(
            key,
            &outbe_gratis::ModifyOperation {
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

fn mint_source_gratis(
    storage: StorageHandle<'_>,
    key: &outbe_tee::resident::AccountKeysHandle,
) -> eyre::Result<()> {
    outbe_gratis::api::mint(
        storage,
        OWNER,
        U256::from(GRATIS),
        source_auth(key, GratisOp::Mint, 0),
    )?;

    Ok(())
}

/// The pledge of the main account for `credis_reservation_id`, sealed as a wallet seals it.
fn sealed(credis_reservation_id: u64) -> Bytes {
    test_enclave::sealed_pledge(
        OWNER,
        U256::from(GRATIS),
        U256::from(credis_reservation_id),
        CHAIN_ID,
    )
    .into()
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

/// The main account's decrypted liquid and pledged Gratis.
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

fn fixture_call(
    evm: &mut EvmWorld,
    caller: Address,
    target: Address,
    call: impl SolCall,
) -> eyre::Result<()> {
    let out = call!(evm, caller, target, U256::ZERO, call);
    assert!(matches!(out.status, SubCallStatus::Success));
    Ok(())
}

fn prepare_issuance_counterparties(evm: &mut EvmWorld, failure: u64) -> eyre::Result<()> {
    for (account, amount) in [(ACCOUNT, AMOUNT), (VAULT_ROUTER_ADDRESS, AMOUNT)] {
        fixture_call(
            evm,
            OWNER,
            ASSET,
            IFixture::mintCall {
                account,
                amount: U256::from(amount),
            },
        )?;
    }
    fixture_call(
        evm,
        OWNER,
        WCOEN_ADDRESS,
        IFixture::mintCall {
            account: VAULT_ROUTER_ADDRESS,
            amount: wcoen(),
        },
    )?;
    fixture_call(
        evm,
        ACCOUNT,
        ASSET,
        IFixture::approveCall {
            spender: VAULT_ROUTER_ADDRESS,
            amount: U256::MAX,
        },
    )?;
    fixture_call(
        evm,
        VAULT_ROUTER_ADDRESS,
        ASSET,
        IFixture::approveCall {
            spender: VAULT,
            amount: U256::MAX,
        },
    )?;
    if [1, 2, 6, 7].contains(&failure) {
        fixture_call(
            evm,
            OWNER,
            ASSET,
            IFixture::configureCall {
                mode: U256::from(failure),
            },
        )?;
    }

    Ok(())
}

fn reset_counterparties(evm: &mut EvmWorld) -> eyre::Result<()> {
    for target in [ASSET, VAULT] {
        call!(
            evm,
            OWNER,
            target,
            U256::ZERO,
            IFixture::configureCall { mode: U256::ZERO }
        );
    }

    Ok(())
}

fn issue(
    evm: &mut EvmWorld,
    pledge: &Bytes,
    succeeds: bool,
) -> eyre::Result<outbe_primitives::storage::SubCallOutput> {
    let issue = ICredisFactory::issueCredisCall {
        credisReservationId: U256::ONE,
        pledge: pledge.clone(),
    };
    assert!(!matches!(
        call!(evm, CCA, CREDIS_FACTORY_ADDRESS, U256::ZERO, issue.clone()).status,
        SubCallStatus::Success
    ));
    let out = call!(evm, ACCOUNT, CREDIS_FACTORY_ADDRESS, U256::ZERO, issue);
    assert_eq!(
        matches!(out.status, SubCallStatus::Success),
        succeeds,
        "{:?} {}",
        out.status,
        String::from_utf8_lossy(&out.returndata)
    );

    Ok(out)
}

fn token_balance(evm: &mut EvmWorld, token: Address, account: Address) -> eyre::Result<U256> {
    let balance = call!(
        evm,
        OWNER,
        token,
        U256::ZERO,
        IFixture::balanceOfCall { account }
    );
    Ok(IFixture::balanceOfCall::abi_decode_returns(
        &balance.returndata,
    )?)
}

fn assert_issuance_state(evm: &mut EvmWorld, issued: bool) -> eyre::Result<()> {
    let moved = |amount: u64| if issued { amount } else { 0 };
    for (account, expected) in [
        (ACCOUNT, AMOUNT - moved(AMOUNT)),
        (CCA, moved(AMOUNT)),
        (OMNIBUS, moved(AMOUNT)),
        (VAULT_ROUTER_ADDRESS, AMOUNT - moved(AMOUNT)),
        (VAULT, 0),
    ] {
        assert_eq!(token_balance(evm, ASSET, account)?, U256::from(expected));
    }
    let wcoen_moved = if issued { wcoen() } else { U256::ZERO };
    assert_eq!(token_balance(evm, WCOEN_ADDRESS, ACCOUNT)?, wcoen_moved);
    assert_eq!(
        token_balance(evm, WCOEN_ADDRESS, VAULT_ROUTER_ADDRESS)?,
        wcoen() - wcoen_moved
    );
    let reservation = call!(
        evm,
        OWNER,
        VAULT_ROUTER_ADDRESS,
        U256::ZERO,
        IVaultRouter::getCredisReservationCall {
            credisReservationId: U256::ONE
        }
    );
    let held = IVaultRouter::getCredisReservationCall::abi_decode_returns(&reservation.returndata)?;
    assert_eq!(held.amountMinor.is_zero(), issued);
    let pledged = if issued {
        U256::from(GRATIS)
    } else {
        U256::ZERO
    };
    assert_eq!(source_gratis(evm)?, (U256::from(GRATIS) - pledged, pledged));
    let redeemed = evm
        .logs()
        .iter()
        .filter(|log| IVaultRouter::CredisReservationRedeemed::decode_log_data(&log.data).is_ok())
        .count();
    assert_eq!(redeemed, usize::from(issued));

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
    let logs = evm.logs().len();
    let payment = call!(
        evm,
        CCA,
        CREDIS_FACTORY_ADDRESS,
        U256::ZERO,
        ICredisFactory::settleCredisCall {
            credisId: observation.credis_id,
            asset: Address::ZERO,
            amountCapMinor: U256::from(1_000_000)
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
    assert_eq!(evm.logs().len(), logs);
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
    use outbe_system_common::addresses::CREDIS_ADDRESS;
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
            spender: VAULT_ROUTER_ADDRESS,
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
    for mode in [1, 2, 3, 9] {
        assert_payment_rollback(evm, mode, &payment)?;
    }
    let paid = call!(
        evm,
        CCA,
        CREDIS_FACTORY_ADDRESS,
        U256::ZERO,
        ICredisFactory::settleCredisCall {
            credisId: credis_id,
            asset: Address::ZERO,
            amountCapMinor: U256::from(1_000_000)
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

fn assert_payment_credis_and_pledge_unchanged(
    evm: &mut EvmWorld,
    observation: &PaymentObservation,
) -> eyre::Result<()> {
    use outbe_system_common::addresses::CREDIS_ADDRESS;
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
