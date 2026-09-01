use std::time::Instant;

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolValue};
use outbe_primitives::{
    addresses::CREDIS_FACTORY_ADDRESS,
    storage::{hashmap::HashMapStorageProvider, Bytecode, StorageHandle},
};
use outbe_vaultrouter::{api::IVaultRouter, VaultRouterContract};

use super::support::{capture_execution, elapsed_ns};
use crate::{
    BenchmarkScenario, ChildFrameFidelity, ChildFrameTrace, ExecutionClass, ExecutionLayer,
    GasLedger, Observation, Profile, ScenarioFidelity, ScenarioMetadata,
};

const CHAIN_ID: u64 = 1;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const ADMIN: Address = Address::repeat_byte(0xa1);
const ASSET: Address = Address::repeat_byte(0x88);
const VAULT: Address = Address::repeat_byte(0x77);
const SMART_ACCOUNT_STUB: Address = Address::repeat_byte(0xaa);
const AMOUNT: u64 = 2_000_000;

sol! {
    interface IERC20Boundary {
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }

    interface IVaultV2Boundary {
        function asset() external view returns (address);
        function owner() external view returns (address);
        function previewWithdraw(uint256 assets) external view returns (uint256 shares);
        function withdraw(uint256 assets, address receiver, address onBehalf) external returns (uint256 shares);
    }

    interface IReferenceCurrencyBoundary {
        function isoCode() external view returns (uint16);
    }

    interface ITokenBundleBoundary {
        function topUp(address sender, address token, uint256 amount) external;
    }
}

pub struct CredisVaultBoundaryScenario;

pub struct PreparedCredisVaultBoundary {
    provider: HashMapStorageProvider,
    calldata: Vec<u8>,
}

fn stub_frame(label: &str, target: Address, selector: [u8; 4]) -> ChildFrameTrace {
    ChildFrameTrace {
        label: label.to_owned(),
        target: format!("{target:#x}"),
        selector: format!("0x{}", hex::encode(selector)),
        status: "success (deterministic benchmark stub)".to_owned(),
        gas_used: 0,
        fidelity: ChildFrameFidelity::BenchmarkStub,
    }
}

impl BenchmarkScenario for CredisVaultBoundaryScenario {
    type Prepared = PreparedCredisVaultBoundary;

    fn metadata(&self) -> ScenarioMetadata {
        ScenarioMetadata::new(
            "credis/create/request/rust-vault-boundary/partial-stubbed/single",
            "Credis request Rust VaultRouter boundary (PARTIAL / STUBBED)",
            ExecutionClass::UserTransaction,
            Profile::Single,
        )
        .with_execution_layer(ExecutionLayer::Marginal)
        .with_fidelity(ScenarioFidelity::PartialStubbed)
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.stub_sub_call_at_selector(
            VAULT,
            IVaultV2Boundary::assetCall::SELECTOR,
            Bytes::from((ASSET,).abi_encode_params()),
        );
        provider.stub_sub_call_at_selector(
            VAULT,
            IVaultV2Boundary::ownerCall::SELECTOR,
            Bytes::from((Address::ZERO,).abi_encode_params()),
        );
        provider.stub_sub_call_at_selector(
            ASSET,
            IReferenceCurrencyBoundary::isoCodeCall::SELECTOR,
            Bytes::from((840_u16,).abi_encode_params()),
        );
        provider.stub_sub_call_at_selector(
            ASSET,
            IERC20Boundary::approveCall::SELECTOR,
            Bytes::new(),
        );
        provider.stub_sub_call_at_selector(
            VAULT,
            IVaultV2Boundary::previewWithdrawCall::SELECTOR,
            Bytes::from((U256::from(AMOUNT),).abi_encode_params()),
        );
        provider.stub_sub_call_at_selector(
            VAULT,
            IERC20Boundary::balanceOfCall::SELECTOR,
            Bytes::from((U256::from(AMOUNT),).abi_encode_params()),
        );
        provider.stub_sub_call_at_selector(
            VAULT,
            IVaultV2Boundary::withdrawCall::SELECTOR,
            Bytes::from((U256::from(AMOUNT),).abi_encode_params()),
        );
        provider.stub_sub_call_at_selector(
            SMART_ACCOUNT_STUB,
            ITokenBundleBoundary::topUpCall::SELECTOR,
            Bytes::new(),
        );

        StorageHandle::enter(&mut provider, |storage| {
            VaultRouterContract::new(storage.clone())
                .owner
                .write(ADMIN)
                .map_err(|error| error.to_string())?;
            storage
                .set_code(
                    SMART_ACCOUNT_STUB,
                    Bytecode::new_raw(Bytes::from_static(&[0x00])),
                )
                .map_err(|error| error.to_string())?;
            outbe_vaultrouter::runtime::add_vault(storage.clone(), ADMIN, VAULT)
                .map_err(|error| error.to_string())?;
            outbe_vaultrouter::runtime::add_liquidity_target(
                storage,
                ADMIN,
                CREDIS_FACTORY_ADDRESS,
                IVaultRouter::StablesTarget::Credis as u8,
            )
            .map_err(|error| error.to_string())
        })?;

        let calldata = IVaultRouter::withdrawCall {
            asset: ASSET,
            amount: U256::from(AMOUNT),
            receiver: SMART_ACCOUNT_STUB,
        }
        .abi_encode();
        Ok(PreparedCredisVaultBoundary { provider, calldata })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        let mut provider = prepared.provider.clone();
        provider.set_gas_limit(BLOCK_GAS_LIMIT);
        provider.enable_production_storage_gas_metering();
        provider.enable_storage_trace();
        let event_offset = provider.get_ordered_events().len();
        let started = Instant::now();
        let output = StorageHandle::enter(&mut provider, |storage| {
            outbe_vaultrouter::precompile::dispatch(
                storage,
                &prepared.calldata,
                CREDIS_FACTORY_ADDRESS,
                U256::ZERO,
            )
            .map_err(|error| error.to_string())
        })?;
        let latency_ns = elapsed_ns(started);
        let burned_shares = IVaultRouter::withdrawCall::abi_decode_returns(&output)
            .map_err(|error| format!("decode VaultRouter withdraw: {error}"))?;
        if burned_shares != U256::from(AMOUNT) {
            return Err(format!(
                "stubbed vault burned-share mismatch: {burned_shares}"
            ));
        }
        let runtime_gas = StorageHandle::enter(&mut provider, |storage| {
            storage.gas_used().map_err(|error| error.to_string())
        })?;
        let captured = capture_execution(
            &provider,
            event_offset,
            GasLedger::UserTransaction,
            runtime_gas,
            "vault_router",
        )?;
        let mut observation = Observation::new(
            [(GasLedger::UserTransaction, captured.gas_total)],
            captured.gas_components,
        )
        .with_total_latency(latency_ns)
        .with_latency("chain.vault_router.withdraw_partial", latency_ns)
        .with_postcondition("vault_router.burned_shares", burned_shares.to_string())
        .with_postcondition(
            "credis.full_smart_account_gas",
            "unavailable; TODO outbe-chain-6le.5",
        )
        .with_postcondition("credis.partial_result_is_production_total", "false");
        observation.storage = captured.storage;
        observation.events = captured.events;
        observation.child_frames = vec![
            stub_frame(
                "IVaultV2.previewWithdraw",
                VAULT,
                IVaultV2Boundary::previewWithdrawCall::SELECTOR,
            ),
            stub_frame(
                "IVaultV2.balanceOf",
                VAULT,
                IERC20Boundary::balanceOfCall::SELECTOR,
            ),
            stub_frame(
                "IVaultV2.withdraw",
                VAULT,
                IVaultV2Boundary::withdrawCall::SELECTOR,
            ),
            stub_frame(
                "IERC20.approve",
                ASSET,
                IERC20Boundary::approveCall::SELECTOR,
            ),
            stub_frame(
                "ITokenBundle.topUp (smart-account TODO)",
                SMART_ACCOUNT_STUB,
                ITokenBundleBoundary::topUpCall::SELECTOR,
            ),
        ];
        Ok(observation)
    }
}
