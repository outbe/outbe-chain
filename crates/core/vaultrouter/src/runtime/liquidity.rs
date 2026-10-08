//! Owner-managed registries of the callers allowed to deposit (sources) and
//! withdraw (targets) liquidity.

use alloy_primitives::{Address, LogData};
use alloy_sol_types::SolEvent;

use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::dsl::{Map, Set};
use outbe_primitives::storage::StorageHandle;

use super::ensure_owner;
use crate::api::IVaultRouter;
use crate::errors::VaultRouterError;
use crate::schema::{VaultRouterContract, UNKNOWN};

fn liquidity_source(value: u8) -> IVaultRouter::StablesSource {
    IVaultRouter::StablesSource::try_from(value).unwrap_or(IVaultRouter::StablesSource::Unknown)
}

fn liquidity_target(value: u8) -> IVaultRouter::StablesTarget {
    IVaultRouter::StablesTarget::try_from(value).unwrap_or(IVaultRouter::StablesTarget::Unknown)
}

#[derive(Clone, Copy)]
enum Registry {
    Sources,
    Targets,
}

impl Registry {
    fn entries<'c, 's>(
        self,
        contract: &'c VaultRouterContract<'s>,
    ) -> (&'c Set<'s, Address>, &'c Map<'s, Address, u8>) {
        match self {
            Self::Sources => (
                &contract.liquidity_sources,
                &contract.liquidity_source_types,
            ),
            Self::Targets => (
                &contract.liquidity_targets,
                &contract.liquidity_target_types,
            ),
        }
    }

    fn invalid_kind(self) -> VaultRouterError {
        match self {
            Self::Sources => VaultRouterError::InvalidLiquiditySource,
            Self::Targets => VaultRouterError::InvalidLiquidityTarget,
        }
    }

    fn not_found(self) -> VaultRouterError {
        match self {
            Self::Sources => VaultRouterError::LiquiditySourceNotFound,
            Self::Targets => VaultRouterError::LiquidityTargetNotFound,
        }
    }

    fn added(self, member: Address, kind: u8) -> LogData {
        match self {
            Self::Sources => IVaultRouter::LiquiditySourceAdded {
                sourceAddress: member,
                sourceType: liquidity_source(kind),
            }
            .encode_log_data(),
            Self::Targets => IVaultRouter::LiquidityTargetAdded {
                targetAddress: member,
                targetType: liquidity_target(kind),
            }
            .encode_log_data(),
        }
    }

    fn removed(self, member: Address, kind: u8) -> LogData {
        match self {
            Self::Sources => IVaultRouter::LiquiditySourceRemoved {
                sourceAddress: member,
                sourceType: liquidity_source(kind),
            }
            .encode_log_data(),
            Self::Targets => IVaultRouter::LiquidityTargetRemoved {
                targetAddress: member,
                targetType: liquidity_target(kind),
            }
            .encode_log_data(),
        }
    }
}

fn add_member(
    storage: StorageHandle<'_>,
    sender: Address,
    member: Address,
    kind: u8,
    registry: Registry,
) -> Result<()> {
    ensure_owner(&storage, sender)?;
    if member.is_zero() {
        return Err(VaultRouterError::ZeroAddress.into());
    }
    if kind == UNKNOWN {
        return Err(registry.invalid_kind().into());
    }
    let contract = VaultRouterContract::new(storage.clone());
    let (members, kinds) = registry.entries(&contract);
    members.insert(member)?;
    kinds.write(&member, kind)?;
    storage.emit_event(VAULT_ROUTER_ADDRESS, registry.added(member, kind))
}

fn remove_member(
    storage: StorageHandle<'_>,
    sender: Address,
    member: Address,
    registry: Registry,
) -> Result<()> {
    ensure_owner(&storage, sender)?;
    let contract = VaultRouterContract::new(storage.clone());
    let (members, kinds) = registry.entries(&contract);
    if !members.remove(&member)? {
        return Err(registry.not_found().into());
    }
    let kind = kinds.read(&member)?;
    kinds.clear(&member)?;
    storage.emit_event(VAULT_ROUTER_ADDRESS, registry.removed(member, kind))
}

pub fn add_liquidity_source(
    storage: StorageHandle<'_>,
    sender: Address,
    source: Address,
    source_type: u8,
) -> Result<()> {
    add_member(storage, sender, source, source_type, Registry::Sources)
}

pub fn remove_liquidity_source(
    storage: StorageHandle<'_>,
    sender: Address,
    source: Address,
) -> Result<()> {
    remove_member(storage, sender, source, Registry::Sources)
}

pub fn add_liquidity_target(
    storage: StorageHandle<'_>,
    sender: Address,
    target: Address,
    target_type: u8,
) -> Result<()> {
    add_member(storage, sender, target, target_type, Registry::Targets)
}

pub fn remove_liquidity_target(
    storage: StorageHandle<'_>,
    sender: Address,
    target: Address,
) -> Result<()> {
    remove_member(storage, sender, target, Registry::Targets)
}

/// Resolves the `StablesSource` registered for `caller`. Returns `Unknown`
/// when `caller` is not a registered source.
pub fn registered_liquidity_source(
    storage: &StorageHandle<'_>,
    caller: Address,
) -> Result<IVaultRouter::StablesSource> {
    let contract = VaultRouterContract::new(storage.clone());
    Ok(liquidity_source(
        contract.liquidity_source_types.read(&caller)?,
    ))
}

/// Resolves the `StablesTarget` registered for `caller`. Returns `Unknown`
/// when `caller` is not a registered target.
pub fn registered_liquidity_target(
    storage: &StorageHandle<'_>,
    caller: Address,
) -> Result<IVaultRouter::StablesTarget> {
    let contract = VaultRouterContract::new(storage.clone());
    Ok(liquidity_target(
        contract.liquidity_target_types.read(&caller)?,
    ))
}
