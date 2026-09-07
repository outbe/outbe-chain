use std::time::Instant;

use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{begin_block, ExecutionScope, WwdEntityId};
use outbe_nod::{NodContract, NodIssueParams};
use outbe_ocomp_protocol::{
    list::{ordered_list_root, streaming_ordered_list_membership_proof, OrderedListLimits},
    nod_materialization::NodMaterializationBatchV1,
    profile::poc_schema_limits,
    result::NodActionV1,
    ListKind,
};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_primitives::time::WorldwideDay;

use super::support::{
    capture_execution, elapsed_ns, seed_compressed_entities_genesis, EmptyParentBodies,
};
use crate::{BenchmarkScenario, ExecutionClass, GasLedger, Observation, Profile, ScenarioMetadata};

const CHAIN_ID: u64 = 1;
const BLOCK_GAS_LIMIT: u64 = 30_000_000;
const TARGET_WWD: WorldwideDay = WorldwideDay::new(20_260_812);

#[derive(Clone, Copy)]
enum NodPath {
    Direct,
    Certified,
}

pub struct NodScenario {
    path: NodPath,
    profile: Profile,
}

impl NodScenario {
    #[must_use]
    pub const fn direct(profile: Profile) -> Self {
        Self {
            path: NodPath::Direct,
            profile,
        }
    }

    #[must_use]
    pub const fn certified(profile: Profile) -> Self {
        Self {
            path: NodPath::Certified,
            profile,
        }
    }
}

pub struct PreparedNod {
    provider: HashMapStorageProvider,
    params: Vec<NodIssueParams>,
    certified: Option<CertifiedFixture>,
}

struct CertifiedFixture {
    actions: Vec<NodActionV1>,
    batches: Vec<NodMaterializationBatchV1>,
}

fn item_count(profile: Profile) -> usize {
    match profile {
        Profile::Single => 1,
        Profile::Typical => 10,
    }
}

fn issue_params(index: usize) -> NodIssueParams {
    let owner_byte = u8::try_from(index + 1).expect("benchmark cardinality fits one byte");
    NodIssueParams {
        owner: Address::repeat_byte(owner_byte),
        gratis_load_minor: U256::from(1_000),
        worldwide_day: TARGET_WWD,
        league_id: 1,
        floor_price_minor: U256::from(540),
        entry_price_minor: U256::from(500),
        issuance_currency: 840,
        reference_currency: 840,
    }
}

fn certified_action(index: usize) -> NodActionV1 {
    let ordinal = u32::try_from(index).expect("benchmark cardinality fits u32");
    let owner = Address::from_word(B256::from(U256::from(ordinal + 1)));
    let floor_price_minor = U256::from(500);
    let tribute_id =
        WwdEntityId::from_day_and_digest(TARGET_WWD, B256::from(U256::from(ordinal + 1_000)));
    let nod_id = NodContract::generate_nod_id(owner, TARGET_WWD)
        .expect("benchmark owner and worldwide day form a Nod id");
    NodActionV1 {
        raw_ordinal: ordinal,
        tribute_id: *tribute_id,
        nod_id: *nod_id,
        owner,
        wwd: TARGET_WWD.value(),
        league_id: 1,
        floor_price_minor,
        gratis_load_minor: U256::from(1_000),
        entry_price_minor: U256::from(510),
        cost_amount_minor: U256::ZERO,
        issuance_currency: 840,
        reference_currency: 840,
        issued_at: 1_600_000_000,
        bucket_key: NodContract::bucket_key(TARGET_WWD, floor_price_minor, 840),
    }
}

fn certified_fixture(count: usize) -> Result<CertifiedFixture, String> {
    const SUBTREE_HEIGHT: usize = 3;
    const BATCH_CAPACITY: usize = 1 << SUBTREE_HEIGHT;

    let limits = poc_schema_limits();
    let actions = (0..count).map(certified_action).collect::<Vec<_>>();
    let encoded = actions
        .iter()
        .map(|action| {
            action
                .encode_canonical_record(&limits)
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let root = ordered_list_root(
        ListKind::NodActions,
        &encoded,
        OrderedListLimits::new(512, limits.max_bounded_bytes, 1 << 20),
    )
    .map_err(|error| error.to_string())?;
    let proofs = (0..count)
        .map(|ordinal| {
            streaming_ordered_list_membership_proof(
                ListKind::NodActions,
                count as u32,
                ordinal as u32,
                encoded.iter(),
                limits.max_bounded_bytes,
            )
            .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let tree_height = count
        .next_power_of_two()
        .trailing_zeros()
        .try_into()
        .map_err(|_| "materialization tree height does not fit usize".to_owned())?;
    let effective_subtree_height = SUBTREE_HEIGHT.min(tree_height);
    let batches = (0..count)
        .step_by(BATCH_CAPACITY)
        .map(|first| {
            let end = (first + BATCH_CAPACITY).min(count);
            NodMaterializationBatchV1 {
                queue_sequence: 1,
                first_nod_ordinal: first as u32,
                actions: actions[first..end].to_vec(),
                root_path: proofs[first][effective_subtree_height..].to_vec(),
            }
        })
        .collect();

    let fixture = CertifiedFixture { actions, batches };
    if fixture.actions.is_empty() {
        return Err("certified Nod benchmark population must not be empty".to_owned());
    }
    seed_root_sanity(root);
    Ok(fixture)
}

fn seed_root_sanity(root: B256) {
    assert_ne!(root, B256::ZERO, "certified Nod root must not be zero");
}

fn seed_certified_world(
    storage: &StorageHandle<'_>,
    fixture: &CertifiedFixture,
) -> Result<(), String> {
    let limits = poc_schema_limits();
    let encoded = fixture
        .actions
        .iter()
        .map(|action| {
            action
                .encode_canonical_record(&limits)
                .expect("fixture is valid")
        })
        .collect::<Vec<_>>();
    let root = ordered_list_root(
        ListKind::NodActions,
        &encoded,
        OrderedListLimits::new(512, limits.max_bounded_bytes, 1 << 20),
    )
    .map_err(|error| error.to_string())?;
    let projection = outbe_nod::NodCertifiedGenerationProjection {
        worldwide_day: TARGET_WWD,
        generation: 1,
        job_id: B256::repeat_byte(0x11),
        protocol_bundle_hash: B256::repeat_byte(0x5b),
        program_semantics_hash: B256::repeat_byte(0x22),
        nod_root: root,
        bucket_root: B256::repeat_byte(0x33),
        output_manifest_root: B256::repeat_byte(0x44),
        tribute_count: fixture.actions.len() as u32,
        nod_count: fixture.actions.len() as u32,
        bucket_count: 1,
        nod_amount_total: U256::from(10_000),
        nod_gratis_consumed: U256::from(10_000),
        issued_at: 1_600_000_000,
        next_nod_ordinal: 0,
        last_progress_height: 1,
    };
    let nod = NodContract::new(storage.clone());
    nod.ocomp_target_generation
        .write(&TARGET_WWD, projection.generation)
        .map_err(|error| error.to_string())?;
    nod.ocomp_namespace_root
        .write(&TARGET_WWD, projection.nod_root)
        .map_err(|error| error.to_string())?;
    nod.ocomp_bucket_root
        .write(&TARGET_WWD, projection.bucket_root)
        .map_err(|error| error.to_string())?;
    nod.ocomp_output_manifest_root
        .write(&TARGET_WWD, projection.output_manifest_root)
        .map_err(|error| error.to_string())?;
    nod.ocomp_generation_metadata
        .write(&TARGET_WWD, projection.metadata_word())
        .map_err(|error| error.to_string())?;
    nod.ocomp_nod_amount_total
        .write(&TARGET_WWD, projection.nod_amount_total)
        .map_err(|error| error.to_string())?;
    nod.ocomp_nod_gratis_consumed
        .write(&TARGET_WWD, projection.nod_gratis_consumed)
        .map_err(|error| error.to_string())?;
    nod.ocomp_materialization_job_id
        .write(&TARGET_WWD, projection.job_id)
        .map_err(|error| error.to_string())?;
    nod.ocomp_materialization_protocol_bundle_hash
        .write(&TARGET_WWD, projection.protocol_bundle_hash)
        .map_err(|error| error.to_string())?;
    nod.ocomp_materialization_program_semantics_hash
        .write(&TARGET_WWD, projection.program_semantics_hash)
        .map_err(|error| error.to_string())?;
    nod.ocomp_materialization_next_nod_ordinal
        .write(&TARGET_WWD, 0)
        .map_err(|error| error.to_string())?;
    nod.ocomp_materialization_last_progress_height
        .write(&TARGET_WWD, 1)
        .map_err(|error| error.to_string())?;
    nod.ocomp_materialization_head_sequence
        .write(1)
        .map_err(|error| error.to_string())?;
    nod.ocomp_materialization_tail_sequence
        .write(2)
        .map_err(|error| error.to_string())?;
    nod.ocomp_materialization_queue_wwd
        .write(&1, TARGET_WWD)
        .map_err(|error| error.to_string())?;

    let config_owner = Address::repeat_byte(0x40);
    let validator = Address::repeat_byte(0x41);
    let delegate = Address::repeat_byte(0x42);
    let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    validators
        .config_owner
        .write(config_owner)
        .map_err(|error| error.to_string())?;
    validators
        .set_config_max_validators(128)
        .map_err(|error| error.to_string())?;
    validators
        .config_epoch_length_blocks
        .write(60)
        .map_err(|error| error.to_string())?;
    validators
        .config_is_initialized
        .write(true)
        .map_err(|error| error.to_string())?;
    validators
        .register_validator(config_owner, validator, &[0x51; 48])
        .map_err(|error| error.to_string())?;
    validators
        .activate_validator_via_boundary_for_test(validator)
        .map_err(|error| error.to_string())?;
    validators
        .set_delegate(
            validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp,
            delegate,
        )
        .map_err(|error| error.to_string())
}

impl BenchmarkScenario for NodScenario {
    type Prepared = PreparedNod;

    fn metadata(&self) -> ScenarioMetadata {
        let profile_name = match self.profile {
            Profile::Single => "single",
            Profile::Typical => "typical-10",
        };
        match self.path {
            NodPath::Direct => ScenarioMetadata::new(
                format!("nod/create/direct/{profile_name}"),
                format!("Nod direct creation ({profile_name})"),
                ExecutionClass::InternalTransition,
                self.profile,
            ),
            NodPath::Certified => ScenarioMetadata::new(
                format!("nod/create/certified/{profile_name}"),
                format!("Nod certified materialization ({profile_name})"),
                ExecutionClass::SystemTransaction,
                self.profile,
            ),
        }
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        provider.set_block_number(1);
        provider.set_timestamp(U256::from(1_700_000_000));
        let count = item_count(self.profile);
        let certified = match self.path {
            NodPath::Direct => None,
            NodPath::Certified => Some(certified_fixture(count)?),
        };
        StorageHandle::enter(&mut provider, |storage| {
            seed_compressed_entities_genesis(&storage)?;
            if let Some(fixture) = &certified {
                seed_certified_world(&storage, fixture)?;
            }
            Ok::<_, String>(())
        })?;
        let params = (0..count).map(issue_params).collect();
        Ok(PreparedNod {
            provider,
            params,
            certified,
        })
    }

    fn run_once(&self, prepared: &Self::Prepared) -> Result<Observation, String> {
        match self.path {
            NodPath::Direct => measure_direct(prepared),
            NodPath::Certified => measure_certified(prepared),
        }
    }
}

fn measure_certified(prepared: &PreparedNod) -> Result<Observation, String> {
    let fixture = prepared
        .certified
        .as_ref()
        .ok_or_else(|| "certified Nod fixture is missing".to_owned())?;
    let mut provider = prepared.provider.clone();
    let scope = StorageHandle::enter(&mut provider, |storage| {
        let scope = ExecutionScope::new();
        begin_block(storage, &scope).map_err(|error| error.to_string())?;
        Ok::<_, String>(scope)
    })?;
    provider.set_gas_limit(BLOCK_GAS_LIMIT);
    provider.enable_production_storage_gas_metering();
    provider.enable_storage_trace();
    let event_offset = provider.get_ordered_events().len();

    let started = Instant::now();
    let mut completed = false;
    for (batch_index, batch) in fixture.batches.iter().enumerate() {
        provider.set_block_number((batch_index + 1) as u64);
        completed = StorageHandle::enter(&mut provider, |storage| {
            outbe_nodfactory::api::materialize_certified_nods(
                &storage,
                &scope,
                &EmptyParentBodies,
                Address::repeat_byte(0x42),
                batch,
                &poc_schema_limits(),
            )
            .map(|outcome| outcome.completed)
            .map_err(|error| error.to_string())
        })?;
    }
    let runtime_gas = StorageHandle::enter(&mut provider, |storage| {
        storage.gas_used().map_err(|error| error.to_string())
    })?;
    let latency_ns = elapsed_ns(started);
    let captured = capture_execution(
        &provider,
        event_offset,
        GasLedger::SystemVisible,
        runtime_gas,
        "nod_materialization",
    )?;

    let all_readable = StorageHandle::enter(&mut provider, |storage| {
        fixture.actions.iter().try_fold(true, |all, action| {
            let id = WwdEntityId::from(action.nod_id);
            outbe_nod::api::get_item(&storage, &scope, &EmptyParentBodies, id)
                .map(|item| all && item.is_some())
                .map_err(|error| error.to_string())
        })
    })?;
    if !completed || !all_readable {
        return Err(
            "certified Nod materialization did not reach its canonical final state".to_owned(),
        );
    }

    let mut observation = Observation::new(
        [(GasLedger::SystemVisible, captured.gas_total)],
        captured.gas_components,
    )
    .with_total_latency(latency_ns)
    .with_latency("chain.nod.materialize_certified", latency_ns)
    .with_postcondition("nod.created_count", fixture.actions.len().to_string())
    .with_postcondition(
        "nod.materialization_batches",
        fixture.batches.len().to_string(),
    )
    .with_postcondition("nod.materialization_completed", "true")
    .with_postcondition("nod.all_readable", "true");
    observation.storage = captured.storage;
    observation.events = captured.events;
    Ok(observation)
}

fn measure_direct(prepared: &PreparedNod) -> Result<Observation, String> {
    let mut provider = prepared.provider.clone();
    let scope = StorageHandle::enter(&mut provider, |storage| {
        let scope = ExecutionScope::new();
        begin_block(storage, &scope).map_err(|error| error.to_string())?;
        Ok::<_, String>(scope)
    })?;
    provider.set_gas_limit(BLOCK_GAS_LIMIT);
    provider.enable_production_storage_gas_metering();
    provider.enable_storage_trace();
    let event_offset = provider.get_ordered_events().len();

    let started = Instant::now();
    let (ids, runtime_gas) = StorageHandle::enter(&mut provider, |storage| {
        let mut ids = Vec::with_capacity(prepared.params.len());
        for params in &prepared.params {
            ids.push(
                outbe_nodfactory::api::issue_nod(&storage, &scope, &EmptyParentBodies, params)
                    .map_err(|error| error.to_string())?,
            );
        }
        let gas = storage.gas_used().map_err(|error| error.to_string())?;
        Ok::<_, String>((ids, gas))
    })?;
    let latency_ns = elapsed_ns(started);
    let captured = capture_execution(
        &provider,
        event_offset,
        GasLedger::SystemInternal,
        runtime_gas,
        "compressed_entities",
    )?;

    let all_readable = StorageHandle::enter(&mut provider, |storage| {
        ids.iter().try_fold(true, |all, id| {
            outbe_nod::api::get_item(&storage, &scope, &EmptyParentBodies, *id)
                .map(|item| all && item.is_some())
                .map_err(|error| error.to_string())
        })
    })?;
    if !all_readable {
        return Err("at least one created Nod is not canonically readable".to_owned());
    }

    let mut observation = Observation::new(
        [(GasLedger::SystemInternal, captured.gas_total)],
        captured.gas_components,
    )
    .with_total_latency(latency_ns)
    .with_latency("chain.nod.issue_direct", latency_ns)
    .with_postcondition("nod.created_count", ids.len().to_string())
    .with_postcondition("nod.all_readable", "true");
    observation.storage = captured.storage;
    observation.events = captured.events;
    Ok(observation)
}
