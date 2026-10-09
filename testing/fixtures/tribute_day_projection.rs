//! Day-directory projection setup for the Tribute day tests.

#[path = "tribute_body.rs"]
pub(crate) mod tribute_body;

use std::{path::Path, sync::Arc};

use alloy_primitives::{Address, Bytes, B256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{
    derive_poseidon_entity_id, encode_tribute_v1, ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_offchain_data::{
    DayDatabaseRoute, FinalizedLog, OffchainDataProjection, ProjectionConfig,
    TributeRetentionSelector,
};
use outbe_offchain_storage::{DayDatabases, RocksDbStorage};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{canonical_body, precompile::ITribute, RetainedTributePin, TributeData};

use tribute_body::{tribute_body, tribute_commitment};

/// The day databases and the shared store under one root.
/// The root value is dropped first, as it is the first field.
pub(crate) struct Store<D> {
    _root: D,
    pub(crate) databases: Arc<DayDatabases>,
    pub(crate) shared: Arc<RocksDbStorage>,
}

pub(crate) fn open<D: AsRef<Path>>(root: D) -> Store<D> {
    let databases = Arc::new(DayDatabases::open(root.as_ref()).unwrap());
    let shared = Arc::new(databases.directory().open_shared().unwrap());
    Store {
        _root: root,
        databases,
        shared,
    }
}

/// Opens the projection on chain 91 and routes it to the day databases.
/// A pin opens it with a selector that retains the pinned day.
pub(crate) fn projection<D>(
    store: &Store<D>,
    start: u64,
    pin: Option<RetainedTributePin>,
) -> OffchainDataProjection {
    let config = ProjectionConfig {
        chain_id: 91,
        genesis_hash: B256::repeat_byte(0x91),
        start_block: start,
    };
    let mut projection = match pin {
        Some(pin) => outbe_offchain_data::open_projection_with_retention_selector(
            config,
            store.shared.clone(),
            store.shared.clone(),
            Arc::new(FixedPin(pin)),
        )
        .unwrap(),
        None => {
            outbe_offchain_data::open_projection(config, store.shared.clone(), store.shared.clone())
                .unwrap()
        }
    };
    projection
        .set_day_route(DayDatabaseRoute {
            databases: store.databases.clone(),
            durable_reader: store.shared.clone(),
            durable_writer: store.shared.clone(),
        })
        .unwrap();
    projection
}

struct FixedPin(RetainedTributePin);

impl TributeRetentionSelector for FixedPin {
    fn active_pin_for(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<RetainedTributePin>, String> {
        Ok((self.0.worldwide_day == worldwide_day).then_some(self.0))
    }
}

pub(crate) fn tribute(owner: Address, day: u32) -> TributeData {
    let tribute_id = derive_poseidon_entity_id(owner, WorldwideDay::new(day)).unwrap();
    tribute_body(tribute_id, owner, day)
}

pub(crate) fn stored_log(body: &TributeData) -> FinalizedLog {
    let payload = encode_tribute_v1(&canonical_body(body)).unwrap();
    FinalizedLog {
        log_index: 0,
        emitter: TRIBUTE_ADDRESS,
        data: ITribute::TributeBodyStored {
            tributeId: body.tribute_id.to_u256(),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: BODY_SCHEMA_V1,
            previousCommitment: B256::ZERO,
            newCommitment: tribute_commitment(body),
            canonicalPayload: Bytes::from(payload),
        }
        .encode_log_data(),
    }
}
