//! Durable OCOMP-owned projection of finalized public block receipts.

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use alloy_primitives::B256;
use outbe_common::WorldwideDay;
use outbe_ocomp_protocol::intent::VerifiedFinalizedIntentV1;
use outbe_offchain_data::{
    OffchainDataProjection, ProjectionConfig, ProjectionOutcome, TributeRetentionSelector,
};
use outbe_offchain_storage::{
    MongoStorage, MongoStorageConfig, MongoWriterLease, StorageError, StorageErrorKind,
    StorageReaderHandle, StorageWriterHandle,
};
use outbe_tribute::RetainedTributePin;
use thiserror::Error;

use crate::{
    exporter::FinalizedTributeSource,
    public_rpc::{PublicOcompRpcClientV1, PublicRpcError},
};

#[derive(Clone, Debug)]
pub struct RpcProjectionConfigV1 {
    pub rpc_url: String,
    pub rpc_max_response_bytes: usize,
    pub mongo: MongoStorageConfig,
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub start_block: u64,
    pub tribute_page_limit: usize,
}

#[derive(Default)]
struct ActiveRetentionPins(RwLock<BTreeMap<WorldwideDay, RetainedTributePin>>);

impl ActiveRetentionPins {
    fn install(&self, finalized: &VerifiedFinalizedIntentV1) -> Result<RetainedTributePin, String> {
        let pin = RetainedTributePin {
            input_lease_id: finalized
                .intent
                .input_lease_id()
                .map_err(|error| error.to_string())?,
            worldwide_day: WorldwideDay::new(finalized.intent.wwd),
        };
        self.0
            .write()
            .map_err(|_| "OCOMP retention pin registry is poisoned".to_owned())?
            .insert(pin.worldwide_day, pin);
        Ok(pin)
    }
}

impl TributeRetentionSelector for ActiveRetentionPins {
    fn active_pin_for(
        &self,
        worldwide_day: WorldwideDay,
    ) -> Result<Option<RetainedTributePin>, String> {
        self.0
            .read()
            .map(|pins| pins.get(&worldwide_day).copied())
            .map_err(|_| "OCOMP retention pin registry is poisoned".to_owned())
    }
}

pub struct RpcFinalizedProjectionV1 {
    rpc: PublicOcompRpcClientV1,
    projection: OffchainDataProjection,
    source: FinalizedTributeSource,
    pins: Arc<ActiveRetentionPins>,
    _writer_lease: MongoWriterLease,
}

impl RpcFinalizedProjectionV1 {
    pub fn open(config: RpcProjectionConfigV1) -> Result<Self, RpcProjectionErrorV1> {
        if config.start_block == 0 {
            return Err(RpcProjectionErrorV1::InvalidConfig);
        }
        let rpc = PublicOcompRpcClientV1::new(config.rpc_url, config.rpc_max_response_bytes)?;
        let storage = Arc::new(MongoStorage::connect(config.mongo).map_err(storage_error)?);
        storage
            .verify_transaction_support()
            .map_err(storage_error)?;
        let writer_lease = storage.acquire_writer_lease().map_err(storage_error)?;
        let reader: StorageReaderHandle = storage.clone();
        let writer: StorageWriterHandle = storage;
        let pins = Arc::new(ActiveRetentionPins::default());
        let projection = OffchainDataProjection::open_with_retention_selector(
            ProjectionConfig {
                chain_id: config.chain_id,
                genesis_hash: config.genesis_hash,
                start_block: config.start_block,
            },
            reader.clone(),
            writer,
            pins.clone(),
        )
        .map_err(projection_error)?;
        let source = FinalizedTributeSource::new(reader, config.tribute_page_limit)
            .map_err(|error| RpcProjectionErrorV1::Projection(error.to_string()))?;
        Ok(Self {
            rpc,
            projection,
            source,
            pins,
            _writer_lease: writer_lease,
        })
    }

    pub fn install_retention_pin(
        &self,
        finalized: &VerifiedFinalizedIntentV1,
    ) -> Result<RetainedTributePin, RpcProjectionErrorV1> {
        self.pins
            .install(finalized)
            .map_err(RpcProjectionErrorV1::Projection)
    }

    pub fn project_through(&mut self, finalized_height: u64) -> Result<u64, RpcProjectionErrorV1> {
        self.project_through_observing(finalized_height, |_| {})
    }

    pub fn project_through_observing(
        &mut self,
        finalized_height: u64,
        mut on_durable_block: impl FnMut(u64),
    ) -> Result<u64, RpcProjectionErrorV1> {
        let mut next = match self.projection.state().checkpoint {
            Some(checkpoint) => checkpoint
                .block_number
                .checked_add(1)
                .ok_or(RpcProjectionErrorV1::HeightOverflow)?,
            None => self.projection.state().start_block,
        };
        while next <= finalized_height {
            let block = self.rpc.finalized_block_receipts(next)?;
            match self
                .projection
                .project_block(&block)
                .map_err(projection_error)?
            {
                ProjectionOutcome::Applied { .. } | ProjectionOutcome::AlreadyApplied(_) => {}
            }
            on_durable_block(next);
            next = next
                .checked_add(1)
                .ok_or(RpcProjectionErrorV1::HeightOverflow)?;
        }
        Ok(self
            .projection
            .state()
            .checkpoint
            .map_or(0, |checkpoint| checkpoint.block_number))
    }

    #[must_use]
    pub const fn tribute_source(&self) -> &FinalizedTributeSource {
        &self.source
    }
}

fn storage_error(error: StorageError) -> RpcProjectionErrorV1 {
    if error.kind() == StorageErrorKind::Unavailable {
        RpcProjectionErrorV1::StorageUnavailable
    } else {
        RpcProjectionErrorV1::Storage(error.to_string())
    }
}

fn projection_error(error: impl std::fmt::Display) -> RpcProjectionErrorV1 {
    RpcProjectionErrorV1::Projection(error.to_string())
}

#[derive(Debug, Error)]
pub enum RpcProjectionErrorV1 {
    #[error(transparent)]
    Rpc(#[from] PublicRpcError),
    #[error("OCOMP RPC projection configuration is invalid")]
    InvalidConfig,
    #[error("OCOMP projection storage is unavailable")]
    StorageUnavailable,
    #[error("OCOMP projection storage failed: {0}")]
    Storage(String),
    #[error("OCOMP finalized receipt projection failed: {0}")]
    Projection(String),
    #[error("OCOMP finalized receipt projection height overflow")]
    HeightOverflow,
}
