//! Atomic encrypted Tribute issuance and the explicit legacy fixture adapter.

use outbe_compressed_entities::{mint, BodyInput, ExecutionScope, ParentBodySource};
use outbe_primitives::error::Result;

use crate::errors::TributeError;
use crate::precompile::ITribute;
use crate::schema::{TributeContract, TributeData};
use crate::TributeRecord;

impl TributeContract<'_> {
    pub fn issue(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute: &TributeData,
    ) -> Result<()> {
        let storage = self.storage_handle();
        storage.with_checkpoint(|| self.issue_inner(scope, parent, tribute))
    }

    /// Issues the enclave-produced ciphertext without exposing individual amounts.
    pub fn issue_encrypted(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        body: &outbe_primitives::tribute_encryption::EncryptedTributeV2,
    ) -> Result<()> {
        if !body.has_valid_encoding() {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(
                    "invalid encrypted Tribute encoding".into(),
                ),
            );
        }
        if body.context.chain_id != self.storage_handle().chain_id()? {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(
                    "Tribute chain identity mismatch".into(),
                ),
            );
        }
        let record = crate::record::from_encrypted(body.clone());
        let expected = outbe_compressed_entities::derive_poseidon_entity_id(
            record.owner,
            record.worldwide_day,
        )
        .map_err(|error| outbe_primitives::error::PrecompileError::Fatal(error.to_string()))?;
        if record.owner.is_zero() || record.tribute_id != expected {
            return Err(TributeError::InvalidOwner.into());
        }
        let storage = self.storage_handle();
        storage.with_checkpoint(|| self.issue_record_inner(scope, parent, &record))
    }

    fn issue_inner(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        tribute: &TributeData,
    ) -> Result<()> {
        crate::state::validate_tribute_for_issue(tribute)?;
        self.issue_record_inner(scope, parent, &crate::record::from_legacy(tribute.clone()))
    }

    fn issue_record_inner(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        record: &TributeRecord,
    ) -> Result<()> {
        self.ensure_day_accepts_tributes(record.worldwide_day)?;
        if self.get_record(scope, parent, record.tribute_id)?.is_some() {
            return Err(TributeError::TributeAlreadyExists.into());
        }
        self.bump_day_bucket_record(record, true)?;
        self.update_pre_admission_for_tribute(record, true)?;
        let supply = self.total_supply.read()?.checked_add(1).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(
                "Tribute total supply overflow during issuance".into(),
            )
        })?;
        self.total_supply.write(supply)?;
        let (issuance, nominal) = match record.encrypted() {
            Some(body) => {
                mint(
                    self.storage_handle(),
                    scope,
                    BodyInput::EncryptedTribute(body),
                )?;
                (
                    body.encrypted_amounts.clone(),
                    body.encrypted_amounts.clone(),
                )
            }
            None => {
                let body = record.calculation_view().map_err(|error| {
                    outbe_primitives::error::PrecompileError::Fatal(error.to_string())
                })?;
                let canonical = crate::repository::canonical_body(&body);
                mint(self.storage_handle(), scope, BodyInput::Tribute(&canonical))?;
                (
                    body.issuance_amount_minor.to_be_bytes::<32>().to_vec(),
                    body.nominal_amount_minor.to_be_bytes::<32>().to_vec(),
                )
            }
        };
        self.emit(ITribute::TributeIssued {
            owner: record.owner,
            tributeId: record.tribute_id.to_u256(),
            worldwideDay: record.worldwide_day.into(),
            issuanceAmountMinor: issuance.into(),
            settlementCurrency: record.issuance_currency,
            nominalAmountMinor: nominal.into(),
        })?;
        Ok(())
    }
}
