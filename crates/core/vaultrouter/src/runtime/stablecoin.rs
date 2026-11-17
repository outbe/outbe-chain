//! Governance-managed registries of the callers allowed to deposit (sources) and
//! withdraw (targets) stablecoins.

use alloy_primitives::{Address, LogData};
use alloy_sol_types::SolEvent;

use outbe_primitives::error::Result;
use outbe_primitives::storage::dsl::{Map, Set};
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::VAULT_ROUTER_ADDRESS;

use crate::errors::VaultRouterError;
use crate::precompile::IVaultRouter;
use crate::schema::{VaultRouterContract, UNKNOWN};

fn stablecoin_source(value: u8) -> IVaultRouter::StablecoinSource {
    IVaultRouter::StablecoinSource::try_from(value)
        .unwrap_or(IVaultRouter::StablecoinSource::Unknown)
}

fn stablecoin_target(value: u8) -> IVaultRouter::StablecoinTarget {
    IVaultRouter::StablecoinTarget::try_from(value)
        .unwrap_or(IVaultRouter::StablecoinTarget::Unknown)
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
                &contract.stablecoin_sources,
                &contract.stablecoin_source_types,
            ),
            Self::Targets => (
                &contract.stablecoin_targets,
                &contract.stablecoin_target_types,
            ),
        }
    }

    fn invalid_kind(self) -> VaultRouterError {
        match self {
            Self::Sources => VaultRouterError::InvalidStablecoinSource,
            Self::Targets => VaultRouterError::InvalidStablecoinTarget,
        }
    }

    fn not_found(self) -> VaultRouterError {
        match self {
            Self::Sources => VaultRouterError::StablecoinSourceNotFound,
            Self::Targets => VaultRouterError::StablecoinTargetNotFound,
        }
    }

    fn added(self, member: Address, kind: u8) -> LogData {
        match self {
            Self::Sources => IVaultRouter::StablecoinSourceAdded {
                sourceAddress: member,
                sourceType: stablecoin_source(kind),
            }
            .encode_log_data(),
            Self::Targets => IVaultRouter::StablecoinTargetAdded {
                targetAddress: member,
                targetType: stablecoin_target(kind),
            }
            .encode_log_data(),
        }
    }

    fn removed(self, member: Address, kind: u8) -> LogData {
        match self {
            Self::Sources => IVaultRouter::StablecoinSourceRemoved {
                sourceAddress: member,
                sourceType: stablecoin_source(kind),
            }
            .encode_log_data(),
            Self::Targets => IVaultRouter::StablecoinTargetRemoved {
                targetAddress: member,
                targetType: stablecoin_target(kind),
            }
            .encode_log_data(),
        }
    }
}

fn add_member(
    storage: StorageHandle<'_>,
    member: Address,
    kind: u8,
    registry: Registry,
) -> Result<()> {
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

fn remove_member(storage: StorageHandle<'_>, member: Address, registry: Registry) -> Result<()> {
    let contract = VaultRouterContract::new(storage.clone());
    let (members, kinds) = registry.entries(&contract);
    if !members.remove(&member)? {
        return Err(registry.not_found().into());
    }
    let kind = kinds.read(&member)?;
    kinds.clear(&member)?;
    storage.emit_event(VAULT_ROUTER_ADDRESS, registry.removed(member, kind))
}

pub(crate) fn add_stablecoin_source(
    storage: StorageHandle<'_>,
    source: Address,
    source_type: u8,
) -> Result<()> {
    add_member(storage, source, source_type, Registry::Sources)
}

pub(crate) fn remove_stablecoin_source(storage: StorageHandle<'_>, source: Address) -> Result<()> {
    remove_member(storage, source, Registry::Sources)
}

pub(crate) fn add_stablecoin_target(
    storage: StorageHandle<'_>,
    target: Address,
    target_type: u8,
) -> Result<()> {
    add_member(storage, target, target_type, Registry::Targets)
}

pub(crate) fn remove_stablecoin_target(storage: StorageHandle<'_>, target: Address) -> Result<()> {
    remove_member(storage, target, Registry::Targets)
}

/// Resolves the `StablecoinSource` registered for `caller`. Returns `Unknown`
/// when `caller` is not a registered source.
pub fn registered_stablecoin_source(
    storage: &StorageHandle<'_>,
    caller: Address,
) -> Result<IVaultRouter::StablecoinSource> {
    let contract = VaultRouterContract::new(storage.clone());
    Ok(stablecoin_source(
        contract.stablecoin_source_types.read(&caller)?,
    ))
}

/// Resolves the `StablecoinTarget` registered for `caller`. Returns `Unknown`
/// when `caller` is not a registered target.
pub fn registered_stablecoin_target(
    storage: &StorageHandle<'_>,
    caller: Address,
) -> Result<IVaultRouter::StablecoinTarget> {
    let contract = VaultRouterContract::new(storage.clone());
    Ok(stablecoin_target(
        contract.stablecoin_target_types.read(&caller)?,
    ))
}
