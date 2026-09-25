//! Credis fixtures and observations over the same RPC/deployment path as Intex.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolCall, SolEvent};
use outbe_primitives::addresses::{
    CCA_REGISTRY_ADDRESS, CREDIS_ADDRESS, ORACLE_ADDRESS, VAULT_ROUTER_ADDRESS,
};

use crate::internal::{addresses, eth};
use crate::world::{forge, settlement_currency::SettlementCurrency, World};

// Canonical interfaces include event constructors with more than seven arguments.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(extra_derives(Debug, PartialEq))]
        "../../contracts/precompiles/src/ICredis.sol"
    );
    alloy_sol_types::sol!("../../contracts/precompiles/src/ICredisFactory.sol");
    alloy_sol_types::sol!(
        #![sol(extra_derives(Debug, PartialEq))]
        "../../contracts/precompiles/src/ICcaRegistry.sol"
    );
}
pub(crate) use abi::{ICcaRegistry, ICredis, ICredisFactory};

sol! {
    interface IMockAccount {
        function execute(address target, uint256 value, bytes calldata data) external payable returns (bytes);
    }
    interface IFixtureToken {
        function mint(address to, uint256 amount) external;
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }
    interface IFixtureVault {
        function deposit(uint256 assets, address onBehalf) external returns (uint256);
    }
}

pub(crate) const USD: u16 = 840;
pub(crate) const DAY: u64 = 86_400;
pub(crate) const INITIAL_GRATIS: U256 = U256::from_limbs([1_000_000_000, 0, 0, 0]);
pub(crate) const INITIAL_STABLES: U256 = INITIAL_GRATIS;
pub(crate) const LIQUIDITY: U256 = U256::from_limbs([10_000_000_000, 0, 0, 0]);
pub(crate) const PRINCIPAL: U256 = U256::from_limbs([300_000_000, 0, 0, 0]);

#[derive(Debug)]
pub(crate) struct CredisFixture {
    pub user: Address,
    pub cca: Address,
    pub cca_key: String,
    pub account: Address,
    pub currency: SettlementCurrency,
    pub keys: eth::ConfidentialAccountKeys,
    pub reservation: U256,
    pub pledge: B256,
    pub collateral: U256,
    pub position_id: U256,
    pub initial_native: U256,
    pub interest_paid: U256,
}

pub(crate) fn actors() -> (Address, String, Address) {
    let user = forge::DEPLOYER_KEY
        .parse::<PrivateKeySigner>()
        .expect("fixture user key")
        .address();
    let key = format!("{:#x}", keccak256(b"outbe-e2e:credis-cca"));
    let cca = key
        .parse::<PrivateKeySigner>()
        .expect("fixture CCA key")
        .address();
    (user, key, cca)
}

pub(crate) fn deploy_account(url: &str, user: Address, cca: Address) -> Address {
    let output = forge::run_with_ctor(
        &crate::env::environment().repo.join("testing/contracts"),
        &["create", "src/MockSmartAccount.sol:MockSmartAccount"],
        &[&format!("{user:#x}"), &format!("{cca:#x}")],
        &[],
        url,
    )
    .expect("deploy mock Credis account");
    forge::address_from(&output, "Deployed to:").expect("mock account address")
}

pub(crate) fn send<C: SolCall>(
    url: &str,
    to: Address,
    key: &str,
    call: &C,
    value: Option<U256>,
) -> serde_json::Value {
    let outcome =
        eth::send_call_outcome(url, to, key, call, value).expect("submit Credis scenario call");
    assert!(
        outcome.success,
        "{} reverted: {}",
        C::SIGNATURE,
        outcome.receipt
    );
    outcome.receipt
}

pub(crate) fn execute<C: SolCall>(
    url: &str,
    account: Address,
    key: &str,
    to: Address,
    call: &C,
) -> serde_json::Value {
    send(
        url,
        account,
        key,
        &IMockAccount::executeCall {
            target: to,
            value: U256::ZERO,
            data: call.abi_encode().into(),
        },
        None,
    )
}

/// Decode exactly one event from its expected emitter; never accept another contract's log.
pub(crate) fn event<E: SolEvent>(receipt: &serde_json::Value, emitter: Address) -> E {
    let mut found = receipt["logs"]
        .as_array()
        .expect("receipt logs")
        .iter()
        .filter_map(|log| {
            if log["address"].as_str()?.parse::<Address>().ok()? != emitter {
                return None;
            }
            let topics = log["topics"]
                .as_array()?
                .iter()
                .map(|t| t.as_str()?.parse::<B256>().ok())
                .collect::<Option<Vec<_>>>()?;
            if topics.first() != Some(&E::SIGNATURE_HASH) {
                return None;
            }
            let data = log["data"].as_str()?.parse::<Bytes>().ok()?;
            Some(E::decode_raw_log(topics, &data).expect("decode matching event"))
        });
    let value = found.next().expect("expected event missing");
    assert!(found.next().is_none(), "duplicate {} event", E::SIGNATURE);
    value
}

pub(crate) fn read<C: SolCall>(url: &str, to: Address, call: &C, height: u64) -> C::Return
where
    C::Return: Send + 'static,
{
    eth::read_call_at(url, to, call, height).expect("Credis finalized read")
}

#[derive(Debug, PartialEq)]
pub(crate) struct Snapshot {
    pub policy_rate: U256,
    pub reservation: (Address, U256, Address, Address, Address, u64),
    pub liquid: U256,
    pub pledged: U256,
    pub account_stables: U256,
    pub cca_stables: U256,
    pub vault_stables: U256,
    pub router_stables: U256,
    pub shares: U256,
    pub native: U256,
    pub position: Option<ICredis::Position>,
}

/// Every state comparison uses the same finalized height on every validator.
pub(crate) fn snapshot(world: &World) -> Snapshot {
    let f = world.state.credis.as_ref().expect("Credis fixture");
    let head = world
        .rpc
        .head(world.validators.primary_port())
        .expect("primary head");
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&world.validators.committee_ports(), head, 120)
        .expect("Credis checkpoint finalized by every validator");
    let mut snapshots = world.validators.committee_ports().into_iter().map(|port| {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, checkpoint.height)
                .expect("checkpoint"),
            checkpoint
        );
        let url = world.rpc.url(port);
        let h = checkpoint.height;
        assert_eq!(
            read(
                &url,
                CCA_REGISTRY_ADDRESS,
                &ICcaRegistry::getCcaStateCall { cca: f.cca },
                h
            ),
            ICcaRegistry::State::Active
        );
        let liquid = read(
            &url,
            addresses::GRATIS_ADDR,
            &eth::IGratis::balanceOfCall { account: f.user },
            h,
        );
        let pledged = read(
            &url,
            addresses::GRATIS_ADDR,
            &eth::IGratis::pledgedOfCall { account: f.user },
            h,
        );
        let balance = |account| {
            read(
                &url,
                f.currency.asset,
                &IFixtureToken::balanceOfCall { account },
                h,
            )
        };
        let native = eth::raw_json_result(
            &url,
            "eth_getBalance",
            serde_json::json!([format!("{:#x}", f.account), format!("0x{h:x}")]),
        )
        .expect("account native balance at checkpoint");
        let position = (!f.position_id.is_zero()).then(|| {
            let position = read(
                &url,
                CREDIS_ADDRESS,
                &ICredis::getPositionCall {
                    positionId: f.position_id,
                },
                h,
            );
            assert_eq!(
                read(
                    &url,
                    CREDIS_ADDRESS,
                    &ICredis::ownerOfCall {
                        positionId: f.position_id
                    },
                    h
                ),
                f.account
            );
            assert_eq!(
                read(
                    &url,
                    CREDIS_ADDRESS,
                    &ICredis::balanceOfCall {
                        smartAccount: f.account
                    },
                    h
                ),
                U256::from(1)
            );
            position
        });
        let reservation = read(
            &url,
            VAULT_ROUTER_ADDRESS,
            &eth::IVaultRouter::reservationOfCall { id: f.reservation },
            h,
        );
        Snapshot {
            policy_rate: read(
                &url,
                ORACLE_ADDRESS,
                &eth::IOracle::getPolicyRateCall { isoCode: USD },
                h,
            ),
            reservation: (
                reservation.asset,
                reservation.amount,
                reservation.smartAccount,
                reservation.cca,
                reservation.vault,
                reservation.expiresAt,
            ),
            liquid: outbe_tee_enclave::gratis::decrypt_balance(&f.keys.view, f.user, &liquid)
                .expect("decrypt Gratis"),
            pledged: outbe_tee_enclave::gratis::decrypt_pledged(&f.keys.view, f.user, &pledged)
                .expect("decrypt pledged Gratis"),
            account_stables: balance(f.account),
            cca_stables: balance(f.cca),
            vault_stables: balance(f.currency.vault),
            router_stables: balance(VAULT_ROUTER_ADDRESS),
            shares: read(
                &url,
                VAULT_ROUTER_ADDRESS,
                &eth::IVaultRouter::sharesBalanceCall {
                    vault: f.currency.vault,
                },
                h,
            ),
            native: native
                .as_str()
                .expect("native balance hex")
                .parse()
                .expect("native U256"),
            position,
        }
    });
    let first = snapshots.next().expect("nonempty committee");
    for observed in snapshots {
        assert_eq!(observed, first, "Credis state differs across validators");
    }
    first
}
