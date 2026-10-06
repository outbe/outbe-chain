//! Real issuance with confidential note consumption and stateful ERC-20/vault calls.
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use outbe_compressed_entities::ExecutionScope;
use outbe_credisfactory::precompile::ICredisFactory;
use outbe_gratis::enclave_client::test_enclave;
use outbe_gratisfactory::precompile::IGratisFactory;
use outbe_oracle::{api::AddressPair, schema::OracleContract};
use outbe_primitives::{
    addresses::{
        CCA_REGISTRY_ADDRESS, CREDIS_FACTORY_ADDRESS, GRATIS_FACTORY_ADDRESS, VAULT_ROUTER_ADDRESS,
    },
    block::BlockContext,
    chain::CHAIN_ID,
    storage::{direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallStatus},
    time::{previous_date_key, timestamp_to_date_key},
};
use outbe_tee::protocol::{GratisOp, ModifyAuth};
use outbe_tee_enclave::gratis::{derive_modify_key, modify_mac};
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
        outbe_evm::sub_call::run(
            &mut $evm.ctx,
            outbe_evm::sub_call::SubCallEnvironment {
                self_address: $caller,
                outer_is_static: false,
                spec: SpecId::PRAGUE,
                runtime_body_readers: None,
                execution_scope: $evm.scope.clone(),
            },
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

struct IssuanceEvm {
    ctx: alloy_evm::eth::EthEvmContext<CacheDB<EmptyDB>>,
    scope: Arc<ExecutionScope>,
}
struct RetryIssuance<'a> {
    stake: U256,
    issue: ICredisFactory::issueCredisCall,
    proof: Vec<u8>,
    withdrawal: Vec<u8>,
    note: &'a outbe_gratis::client::Note,
}
struct PaymentObservation {
    position: U256,
    position_call: outbe_credis::precompile::ICredis::getPositionCall,
    before: Bytes,
    root: Bytes,
}
#[test]
fn issuance_pays_cca_preserves_account_stables_and_rolls_back_failed_payouts() {
    // Preserve the original success, payout/authorization/metadata failure and expiry cases.
    for failure in [0, 1, 2, 4, 5, 6, 7, 8] {
        test_enclave::install();
        let key = derive_modify_key(&test_enclave::state_key(), OWNER).unwrap();
        let (mut db, stake) = issuance_database().expect("issuance database fixture succeeds");
        let mut provider = DirectStorageProvider::new(
            &mut db,
            BlockContext::new(1, NOW, CHAIN_ID, OWNER, vec![OWNER]),
        );
        let (note, proof, withdrawal) = StorageHandle::enter(&mut provider, |storage| {
            seed_issuance_liquidity(storage.clone())
                .expect("seed issuance liquidity fixture succeeds");
            seed_issuance_oracle(storage.clone()).expect("seed issuance oracle fixture succeeds");
            let (note, commitment) = pledge_issuance_note(storage.clone(), &key)
                .expect("pledge issuance note fixture succeeds");
            let (proof, withdrawal) = prove_issuance_and_withdrawal(storage, &note, commitment)
                .expect("prove issuance and withdrawal fixture succeeds");
            (note, proof, withdrawal)
        });
        provider.flush().unwrap();
        let ctx = Context::mainnet()
            .with_db(db)
            .modify_cfg_chained(|cfg| cfg.chain_id = CHAIN_ID)
            .modify_block_chained(|block| block.timestamp = U256::from(NOW));
        let mut evm = IssuanceEvm {
            ctx,
            scope: Arc::new(ExecutionScope::new()),
        };
        prepare_issuance_counterparties(&mut evm, failure)
            .expect("prepare issuance counterparties fixture succeeds");
        let (issue, out) = issue_cca_payout(&mut evm, failure, stake, &proof)
            .expect("issue cca payout fixture succeeds");
        assert_issuance_state(&mut evm, failure, stake)
            .expect("assert issuance state fixture succeeds");
        retry_issuance_and_cancel_expired(
            &mut evm,
            failure,
            RetryIssuance {
                stake,
                issue,
                proof,
                withdrawal,
                note: &note,
            },
        )
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
    let code = Bytecode::new_raw(Bytes::from(alloy_primitives::hex::decode(
        include_str!("fixtures/CredisIssuance.hex").trim(),
    )?));
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
    router.liquidity_sources.insert(CREDIS_FACTORY_ADDRESS)?;
    router.liquidity_source_types.write(
        &CREDIS_FACTORY_ADDRESS,
        IVaultRouter::StablesSource::CredisCostAmount as u8,
    )?;
    router.liquidity_targets.insert(CREDIS_FACTORY_ADDRESS)?;
    router.liquidity_target_types.write(
        &CREDIS_FACTORY_ADDRESS,
        IVaultRouter::StablesTarget::Credis as u8,
    )?;
    // Reserve before pledging. The code below funds the held assets through real ERC20 calls.
    router.reservations.create(&LiquidityReservation {
        id: U256::ONE,
        asset: ASSET,
        amount: U256::from(2_000_000),
        smart_account: ACCOUNT,
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
    })?;

    Ok(())
}

fn seed_issuance_oracle(storage: StorageHandle<'_>) -> eyre::Result<()> {
    let price = U256::from(2_000_000);
    outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(840))?;
    outbe_oracle::api::set_exchange_rate(
        storage.clone(),
        Address::ZERO,
        AddressPair::new_coen_to(840),
        price,
        1,
        NOW,
    )?;
    let oracle = OracleContract::new(storage.clone());
    oracle.reference_currencies.push(840)?;
    oracle.policy_rate.write(&840, U256::from(43_000))?;
    let day = previous_date_key(timestamp_to_date_key(NOW));
    let index = outbe_oracle::api::coen_pair_index_opt(storage.clone(), 840)?
        .ok_or_else(|| eyre::eyre!("missing fixture value"))?;
    oracle.record_utc_day_vwap(day, index, price)?;
    oracle.utc_day_vwap_last_finalized.write(day)?;

    Ok(())
}

fn pledge_issuance_note(
    storage: StorageHandle<'_>,
    key: &[u8; 32],
) -> eyre::Result<(outbe_gratis::client::Note, B256)> {
    let auth = |op, amount, op_nonce| ModifyAuth {
        mac: modify_mac(
            key,
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
    )?;
    let section = outbe_fidelity::api::cohort_section(
        storage.clone(),
        OWNER,
        outbe_tee::protocol::FidelityCohortOp::Probe,
        NOW,
    )?;
    let (commitment, _) = outbe_gratis::api::pledge_with_fidelity(
        storage.clone(),
        OWNER,
        gratis,
        auth(GratisOp::Pledge, gratis, 1),
        section,
    )?;
    let note = outbe_gratis::client::Note::initial(CHAIN_ID, OWNER, key, gratis, 1)?;
    assert_eq!(note.commitment().unwrap(), commitment);

    Ok((note, commitment))
}

fn prove_issuance_and_withdrawal(
    storage: StorageHandle<'_>,
    note: &outbe_gratis::client::Note,
    commitment: B256,
) -> eyre::Result<(Vec<u8>, Vec<u8>)> {
    let gratis = U256::from(1_000_000);
    let mut tree = outbe_gratis::client::new_tree(CHAIN_ID)?;
    tree.append(outbe_protocol::codec::field_from_b256(&commitment)?)?;
    let reservation = outbe_vaultrouter::api::reservation_of(&storage, U256::ONE)?;
    let context = outbe_credisfactory::runtime::reservation_context(
        CHAIN_ID,
        U256::ONE,
        &reservation.into(),
    )?;
    let proof = outbe_gratis::client::prove_issue(note, &tree, gratis, context)?;
    let withdrawal = outbe_gratis::client::prove_unpledge(
        note,
        &tree,
        gratis,
        outbe_gratis::api::unpledge_context(CHAIN_ID, OWNER, gratis)?,
    )?;

    Ok((proof, withdrawal))
}

fn prepare_issuance_counterparties(evm: &mut IssuanceEvm, failure: u64) -> eyre::Result<()> {
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
    evm: &mut IssuanceEvm,
    failure: u64,
    stake: U256,
    proof: &[u8],
) -> eyre::Result<(
    ICredisFactory::issueCredisCall,
    outbe_primitives::storage::SubCallOutput,
)> {
    let issue = ICredisFactory::issueCredisCall {
        reservationId: U256::ONE,
        proof: if failure == 5 {
            Bytes::new()
        } else {
            proof.to_vec().into()
        },
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

fn assert_issuance_state(evm: &mut IssuanceEvm, failure: u64, stake: U256) -> eyre::Result<()> {
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
    evm: &mut IssuanceEvm,
    failure: u64,
    inputs: RetryIssuance<'_>,
) -> eyre::Result<()> {
    // Reset counterparties and retry with the original note and exact contribution.
    for target in [ASSET, VAULT] {
        call!(
            evm,
            OWNER,
            target,
            U256::ZERO,
            IFixture::configureCall { mode: U256::ZERO }
        );
    }
    // The restored note stays valid through equality, independently of liquidity expiry.
    evm.ctx.block.timestamp = U256::from(NOW + if failure == 8 { 901 } else { 900 });
    let retry = call!(
        evm,
        CCA,
        CREDIS_FACTORY_ADDRESS,
        inputs.stake,
        ICredisFactory::issueCredisCall {
            proof: inputs.proof.into(),
            ..inputs.issue
        }
    );
    assert_eq!(
        matches!(retry.status, SubCallStatus::Success),
        failure != 0 && failure != 8,
        "failed issuance must restore the note and position; success must prevent replay: {:?}",
        retry.status
    );
    if failure == 8 {
        assert!(String::from_utf8_lossy(&retry.returndata).contains("expired"));
        let cancel = IGratisFactory::unpledgeGratisCall {
            proof: inputs.withdrawal.into(),
        };
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
        let spent: Vec<_> = evm
            .ctx
            .journaled_state
            .logs()
            .iter()
            .filter_map(|log| IGratisFactory::PledgeSpent::decode_log_data(&log.data).ok())
            .collect();
        assert_eq!(spent.len(), 1);
        assert_eq!(spent[0].nullifier, inputs.note.nullifier().unwrap());
        assert!(!matches!(
            call!(evm, OWNER, GRATIS_FACTORY_ADDRESS, U256::ZERO, cancel).status,
            SubCallStatus::Success
        ));
    }

    Ok(())
}

fn assert_payment_rollback(
    evm: &mut IssuanceEvm,
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
            positionId: observation.position,
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
    assert_payment_position_and_pledge_unchanged(evm, observation)?;
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
    evm: &mut IssuanceEvm,
    out: &outbe_primitives::storage::SubCallOutput,
) -> eyre::Result<()> {
    use outbe_credis::precompile::ICredis;
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    let position = ICredisFactory::issueCredisCall::abi_decode_returns(&out.returndata)?.positionId;
    call!(
        evm,
        OWNER,
        ASSET,
        U256::ZERO,
        IFixture::setPositionCall { id: position }
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
    let position_call = ICredis::getPositionCall {
        positionId: position,
    };
    let before = call!(
        evm,
        OWNER,
        CREDIS_ADDRESS,
        U256::ZERO,
        position_call.clone()
    )
    .returndata;
    let root = call!(
        evm,
        OWNER,
        GRATIS_FACTORY_ADDRESS,
        U256::ZERO,
        IGratisFactory::pledgeRootCall {}
    )
    .returndata;
    let payment = PaymentObservation {
        position,
        position_call,
        before,
        root: root.clone(),
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
            positionId: position,
            amountMinor: U256::from(1_000_000)
        }
    );
    assert!(
        matches!(paid.status, SubCallStatus::Success),
        "{:?}",
        paid.returndata
    );
    assert_ne!(
        call!(
            evm,
            OWNER,
            GRATIS_FACTORY_ADDRESS,
            U256::ZERO,
            IGratisFactory::pledgeRootCall {}
        )
        .returndata,
        root
    );

    Ok(())
}

fn assert_issuance_native_balances(
    evm: &mut IssuanceEvm,
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

fn assert_issuance_payout_events(evm: &IssuanceEvm, failure: u64) -> eyre::Result<()> {
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

fn assert_payment_position_and_pledge_unchanged(
    evm: &mut IssuanceEvm,
    observation: &PaymentObservation,
) -> eyre::Result<()> {
    use outbe_primitives::addresses::CREDIS_ADDRESS;
    assert_eq!(
        call!(
            evm,
            OWNER,
            CREDIS_ADDRESS,
            U256::ZERO,
            observation.position_call.clone()
        )
        .returndata,
        observation.before
    );
    assert_eq!(
        call!(
            evm,
            OWNER,
            GRATIS_FACTORY_ADDRESS,
            U256::ZERO,
            IGratisFactory::pledgeRootCall {}
        )
        .returndata,
        observation.root
    );

    Ok(())
}

fn assert_payment_token_balances(evm: &mut IssuanceEvm) -> eyre::Result<()> {
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
