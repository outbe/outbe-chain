use std::collections::BTreeMap;
use std::time::Instant;

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    EntityRef, IdPage, IdPageRequest, ParentBodySource, ParentBodySourceError, QueryRef, StoredBody,
};
use outbe_intexfactory::constants::ORIGIN_ROUTER_ADDRESS;
use outbe_primitives::{
    addresses::{
        COMPRESSED_ENTITIES_ADDRESS, CREDIS_ADDRESS, CREDIS_FACTORY_ADDRESS, FIDELITY_ADDRESS,
        GEM_ADDRESS, GEM_FACTORY_ADDRESS, GRATIS_ADDRESS, GRATIS_FACTORY_ADDRESS, INTEX_ADDRESS,
        INTEX_FACTORY_ADDRESS, METADOSIS_ADDRESS, NOD_ADDRESS, NOD_FACTORY_ADDRESS, ORACLE_ADDRESS,
        PROMIS_ADDRESS, PROMIS_FACTORY_ADDRESS, STABLECOIN_FACTORY_ADDRESS,
        STABLECOIN_POLICY_REGISTRY_ADDRESS, TRIBUTE_ADDRESS, TRIBUTE_FACTORY_ADDRESS,
        VALIDATOR_SET_ADDRESS, VAULT_ROUTER_ADDRESS,
    },
    storage::hashmap::{HashMapStorageProvider, StorageTraceKind, StorageTraceOperation},
};
use revm::context_interface::cfg::gas::{SSTORE_RESET, WARM_STORAGE_READ_COST};

use crate::{EventCount, GasComponent, GasLedger, StorageOperationKind, StorageTraceEntry};

pub(crate) struct EmptyParentBodies;

impl ParentBodySource for EmptyParentBodies {
    fn get(&self, _entity: EntityRef) -> Result<Option<StoredBody>, ParentBodySourceError> {
        Ok(None)
    }

    fn list(
        &self,
        _query: QueryRef,
        _request: IdPageRequest,
    ) -> Result<IdPage, ParentBodySourceError> {
        Ok(IdPage {
            ids: Vec::new(),
            next_after: None,
        })
    }
}

pub(crate) fn seed_compressed_entities_genesis(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
) -> Result<(), String> {
    storage
        .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
        .map_err(|error| error.to_string())?;
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(
                outbe_compressed_entities::sealed_root(B256::ZERO)
                    .map_err(|error| error.to_string())?
                    .as_slice(),
            ),
        )
        .map_err(|error| error.to_string())
}

pub(crate) struct CapturedExecution {
    pub gas_total: u64,
    pub gas_components: Vec<GasComponent>,
    pub storage: Vec<StorageTraceEntry>,
    pub events: Vec<EventCount>,
}

pub(crate) fn capture_execution(
    provider: &HashMapStorageProvider,
    event_offset: usize,
    ledger: GasLedger,
    runtime_gas: u64,
    explicit_runtime_module: &str,
) -> Result<CapturedExecution, String> {
    let (reads, writes) = provider.metered_storage_operations();
    let trace = provider.storage_trace();
    let trace_reads = trace
        .iter()
        .filter(|operation| operation.kind == StorageTraceKind::Read)
        .count() as u64;
    let trace_writes = trace
        .iter()
        .filter(|operation| operation.kind == StorageTraceKind::Write)
        .count() as u64;
    if (reads, writes) != (trace_reads, trace_writes) {
        return Err(format!(
            "storage trace differs from production meter: meter=({reads},{writes}), trace=({trace_reads},{trace_writes})"
        ));
    }

    let storage_gas = reads
        .saturating_mul(WARM_STORAGE_READ_COST)
        .saturating_add(writes.saturating_mul(SSTORE_RESET));
    let explicit_runtime_gas = runtime_gas
        .checked_sub(storage_gas)
        .ok_or_else(|| "runtime gas is lower than metered storage gas".to_owned())?;
    let mut gas_components = storage_gas_components(trace, ledger);
    if explicit_runtime_gas > 0 {
        gas_components.push(
            GasComponent::new(
                ledger,
                format!("runtime.{explicit_runtime_module}.explicit"),
                explicit_runtime_gas,
                1,
            )
            .attributed_to(explicit_runtime_module),
        );
    }

    Ok(CapturedExecution {
        gas_total: runtime_gas,
        gas_components,
        storage: aggregate_storage_trace(trace),
        events: aggregate_events(&provider.get_ordered_events()[event_offset..]),
    })
}

pub(crate) fn storage_gas_components(
    trace: &[StorageTraceOperation],
    ledger: GasLedger,
) -> Vec<GasComponent> {
    let mut grouped = BTreeMap::<(&'static str, StorageTraceKind), u64>::new();
    for operation in trace {
        *grouped
            .entry((module_name(operation.address), operation.kind))
            .or_default() += 1;
    }
    grouped
        .into_iter()
        .map(|((module, kind), count)| {
            let (suffix, per_operation) = match kind {
                StorageTraceKind::Read => ("read", WARM_STORAGE_READ_COST),
                StorageTraceKind::Write => ("write", SSTORE_RESET),
            };
            GasComponent::new(
                ledger,
                format!("storage.{module}.{suffix}"),
                count.saturating_mul(per_operation),
                count,
            )
            .attributed_to(module)
        })
        .collect()
}

pub(crate) fn aggregate_storage_trace(trace: &[StorageTraceOperation]) -> Vec<StorageTraceEntry> {
    let mut grouped = BTreeMap::<(String, String, String, StorageOperationKind), u64>::new();
    for operation in trace {
        let kind = match operation.kind {
            StorageTraceKind::Read => StorageOperationKind::Read,
            StorageTraceKind::Write => StorageOperationKind::Write,
        };
        *grouped
            .entry((
                module_name(operation.address).to_owned(),
                format!("{:#x}", operation.address),
                format!("{:#x}", operation.slot),
                kind,
            ))
            .or_default() += 1;
    }
    grouped
        .into_iter()
        .map(
            |((module, address, slot, operation), count)| StorageTraceEntry {
                module,
                address,
                slot,
                operation,
                count,
                gas: count.saturating_mul(match operation {
                    StorageOperationKind::Read => WARM_STORAGE_READ_COST,
                    StorageOperationKind::Write => SSTORE_RESET,
                }),
            },
        )
        .collect()
}

pub(crate) fn aggregate_events(events: &[alloy_primitives::Log]) -> Vec<EventCount> {
    let mut grouped = BTreeMap::<(String, String), u64>::new();
    for event in events {
        let topic = event
            .data
            .topics()
            .first()
            .map_or_else(|| "none".to_owned(), |topic| format!("{topic:#x}"));
        *grouped
            .entry((format!("{:#x}", event.address), topic))
            .or_default() += 1;
    }
    grouped
        .into_iter()
        .map(|((emitter, event), count)| EventCount {
            emitter,
            event,
            count,
        })
        .collect()
}

fn module_name(address: Address) -> &'static str {
    if outbe_primitives::addresses::is_stablecoin_address(address) {
        return "stablecoin";
    }
    match address {
        COMPRESSED_ENTITIES_ADDRESS => "compressed_entities",
        CREDIS_ADDRESS => "credis",
        CREDIS_FACTORY_ADDRESS => "credis_factory",
        FIDELITY_ADDRESS => "fidelity",
        GEM_ADDRESS => "gem",
        GEM_FACTORY_ADDRESS => "gem_factory",
        GRATIS_ADDRESS => "gratis",
        GRATIS_FACTORY_ADDRESS => "gratis_factory",
        INTEX_ADDRESS => "intex",
        INTEX_FACTORY_ADDRESS => "intex_factory",
        METADOSIS_ADDRESS => "metadosis",
        NOD_ADDRESS => "nod",
        NOD_FACTORY_ADDRESS => "nod_factory",
        ORACLE_ADDRESS => "oracle",
        ORIGIN_ROUTER_ADDRESS => "origin_router",
        PROMIS_ADDRESS => "promis",
        PROMIS_FACTORY_ADDRESS => "promis_factory",
        STABLECOIN_FACTORY_ADDRESS => "stablecoin_factory",
        STABLECOIN_POLICY_REGISTRY_ADDRESS => "stablecoin_policy_registry",
        TRIBUTE_ADDRESS => "tribute",
        TRIBUTE_FACTORY_ADDRESS => "tribute_factory",
        VALIDATOR_SET_ADDRESS => "validator_set",
        VAULT_ROUTER_ADDRESS => "vault_router",
        _ => "other",
    }
}

pub(crate) fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
