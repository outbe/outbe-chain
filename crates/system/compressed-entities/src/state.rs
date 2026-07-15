use alloy_primitives::U256;
use outbe_macros::contract;
use outbe_primitives::{
    addresses::COMPRESSED_ENTITIES_ADDRESS,
    error::{PrecompileError, Result},
    storage::types::{Mapping, Slot},
};

use crate::{identity_field, Commitment, EntityId36};

const STORAGE_SCHEMA_VERSION: u64 = 1;

/// ADR-006 direct commitment backend at the system-owned state address.
#[contract(addr = COMPRESSED_ENTITIES_ADDRESS)]
pub struct CommitmentState {
    /// Slot 0: direct-map storage schema.
    pub storage_schema_version: Slot<u64>,
    /// Slot 1: Tribute commitments keyed by `identity_f`.
    tribute_commitments: Mapping<U256, U256>,
    /// Slot 2: Nod item commitments keyed by `identity_f`.
    nod_commitments: Mapping<U256, U256>,
    /// Slot 3: Nod bucket commitments keyed by `identity_f`.
    bucket_commitments: Mapping<U256, U256>,
}

impl CommitmentState<'_> {
    pub fn tribute(&self, identity: EntityId36) -> Result<Option<Commitment>> {
        self.read(&self.tribute_commitments, identity)
    }

    pub fn nod_item(&self, identity: EntityId36) -> Result<Option<Commitment>> {
        self.read(&self.nod_commitments, identity)
    }

    pub fn nod_bucket(&self, identity: EntityId36) -> Result<Option<Commitment>> {
        self.read(&self.bucket_commitments, identity)
    }

    pub fn set_tribute(&self, identity: EntityId36, commitment: Commitment) -> Result<()> {
        self.write(&self.tribute_commitments, identity, commitment)
    }

    pub fn set_nod_item(&self, identity: EntityId36, commitment: Commitment) -> Result<()> {
        self.write(&self.nod_commitments, identity, commitment)
    }

    pub fn set_nod_bucket(&self, identity: EntityId36, commitment: Commitment) -> Result<()> {
        self.write(&self.bucket_commitments, identity, commitment)
    }

    pub fn clear_tribute(&self, identity: EntityId36) -> Result<()> {
        self.clear(&self.tribute_commitments, identity)
    }

    pub fn clear_nod_item(&self, identity: EntityId36) -> Result<()> {
        self.clear(&self.nod_commitments, identity)
    }

    pub fn clear_nod_bucket(&self, identity: EntityId36) -> Result<()> {
        self.clear(&self.bucket_commitments, identity)
    }

    fn read(
        &self,
        mapping: &Mapping<'_, U256, U256>,
        identity: EntityId36,
    ) -> Result<Option<Commitment>> {
        self.validate_schema_for_read()?;
        let key = commitment_key(identity)?;
        let value = mapping.read(&key)?;
        if value.is_zero() {
            return Ok(None);
        }
        Commitment::try_from(value.to_be_bytes::<32>())
            .map(Some)
            .map_err(|error| PrecompileError::Fatal(error.to_string()))
    }

    fn write(
        &self,
        mapping: &Mapping<'_, U256, U256>,
        identity: EntityId36,
        commitment: Commitment,
    ) -> Result<()> {
        self.ensure_schema()?;
        mapping.write(&commitment_key(identity)?, commitment.to_u256())
    }

    fn clear(&self, mapping: &Mapping<'_, U256, U256>, identity: EntityId36) -> Result<()> {
        self.ensure_schema()?;
        mapping.write(&commitment_key(identity)?, U256::ZERO)
    }

    fn validate_schema_for_read(&self) -> Result<()> {
        let actual = self.storage_schema_version.read()?;
        if actual == 0 || actual == STORAGE_SCHEMA_VERSION {
            return Ok(());
        }
        Err(PrecompileError::Fatal(format!(
            "unsupported compressed-entity storage schema {actual}"
        )))
    }

    fn ensure_schema(&self) -> Result<()> {
        let actual = self.storage_schema_version.read()?;
        match actual {
            0 => self.storage_schema_version.write(STORAGE_SCHEMA_VERSION),
            STORAGE_SCHEMA_VERSION => Ok(()),
            _ => Err(PrecompileError::Fatal(format!(
                "unsupported compressed-entity storage schema {actual}"
            ))),
        }
    }
}

fn commitment_key(identity: EntityId36) -> Result<U256> {
    identity_field(identity)
        .map(U256::from_be_bytes)
        .map_err(|error| PrecompileError::Fatal(error.to_string()))
}
