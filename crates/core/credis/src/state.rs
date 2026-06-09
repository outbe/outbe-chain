//! Storage CRUD and per-address index helpers for the Credis contract.
//!
//! All functions take a short-lived `&mut CredisContract` (or `&CredisContract`
//! for reads) constructed via `CredisContract::new(storage)`. They only touch
//! local storage; orchestration logic lives in `runtime.rs`.

use alloy_primitives::{Address, U256};

use outbe_primitives::error::Result;

use crate::errors::CredisError;
use crate::schema::{Anadosis, CredisContract, Position};

impl CredisContract<'_> {
    // ---------------------------------------------------------------------
    // Position CRUD
    // ---------------------------------------------------------------------

    pub(crate) fn position_exists(&self, position_id: U256) -> Result<bool> {
        self.positions.exists(position_id)
    }

    pub(crate) fn load_position(&self, position_id: U256) -> Result<Position> {
        self.positions
            .get(position_id)?
            .ok_or_else(|| CredisError::PositionNotFound.into())
    }

    pub(crate) fn create_position_record(&mut self, position: &Position) -> Result<()> {
        self.positions.create(position)
    }

    pub(crate) fn update_position_record(&mut self, position: &Position) -> Result<()> {
        self.positions.update(position)
    }

    // ---------------------------------------------------------------------
    // Anadosis CRUD
    // ---------------------------------------------------------------------

    pub(crate) fn load_anadosis(
        &self,
        position_id: U256,
        anadosis_number: u32,
    ) -> Result<Anadosis> {
        let key = CredisContract::anadosis_key(position_id, anadosis_number);
        self.anadosis_records
            .get(key)?
            .ok_or_else(|| CredisError::PositionNotFound.into())
    }

    pub(crate) fn create_anadosis_record(&mut self, anadosis: &Anadosis) -> Result<()> {
        self.anadosis_records.create(anadosis)
    }

    pub(crate) fn update_anadosis_record(&mut self, anadosis: &Anadosis) -> Result<()> {
        self.anadosis_records.update(anadosis)
    }

    // ---------------------------------------------------------------------
    // Per-address dense index (mirrors outbe-nod owner_nod_* shape)
    // ---------------------------------------------------------------------

    pub(crate) fn append_to_address_index(
        &mut self,
        account: Address,
        position_id: U256,
    ) -> Result<()> {
        let count = self.address_position_counts.read(&account)?;
        let key = CredisContract::address_index_key(account, count);
        self.address_position_ids.write(&key, position_id)?;
        self.address_position_counts.write(&account, count + 1)?;
        Ok(())
    }

    pub(crate) fn read_address_position_count(&self, account: Address) -> Result<u32> {
        self.address_position_counts.read(&account)
    }

    pub(crate) fn read_address_position_id(&self, account: Address, index: u32) -> Result<U256> {
        let key = CredisContract::address_index_key(account, index);
        self.address_position_ids.read(&key)
    }

    // ---------------------------------------------------------------------
    // Global dense index for getAllPositions iteration
    // ---------------------------------------------------------------------

    pub(crate) fn append_to_global_index(&mut self, position_id: U256) -> Result<()> {
        let total = self.total_positions.read()?;
        self.position_id_at_index.write(&total, position_id)?;
        self.total_positions.write(total + 1)?;
        Ok(())
    }

    pub(crate) fn read_total_positions(&self) -> Result<u64> {
        self.total_positions.read()
    }

    pub(crate) fn read_position_id_at(&self, index: u64) -> Result<U256> {
        self.position_id_at_index.read(&index)
    }
}
