use alloy_primitives::{keccak256, Address, B256, U256};
use base64::Engine;
use outbe_primitives::error::Result;
use outbe_primitives::math::{
    constants::MAX_BIN_ID,
    price_helper,
    tree_math::{self, BinTreeStorage},
};

use crate::{
    constants::{BIN_STEP_BP, TOKEN_DESCRIPTION, TOKEN_IMAGE_BASE, TOKEN_NAME, TOKEN_SYMBOL},
    errors::GemError,
    schema::{GemContract, GemData, GemState},
};

impl GemContract<'_> {
    pub fn name() -> &'static str {
        TOKEN_NAME
    }

    pub fn symbol() -> &'static str {
        TOKEN_SYMBOL
    }

    pub fn format_gem_id(gem_id: U256) -> String {
        hex::encode(gem_id.to_be_bytes::<32>())
    }

    pub fn parse_gem_id(gem_id: &str) -> Result<U256> {
        let trimmed = gem_id.strip_prefix("0x").unwrap_or(gem_id);
        if trimmed.len() != 64 {
            return Err(GemError::GemNotFound.into());
        }
        let mut buf = [0u8; 32];
        hex::decode_to_slice(trimmed, &mut buf).map_err(|_| GemError::GemNotFound)?;
        Ok(U256::from_be_bytes(buf))
    }

    pub fn total_supply(&self) -> Result<u64> {
        self.total_supply.read()
    }

    pub fn balance_of(&self, owner: Address) -> Result<u32> {
        self.owner_gem_counts.read(&owner)
    }

    pub fn owner_of(&self, gem_id: U256) -> Result<Address> {
        let item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        Ok(item.owner)
    }

    pub fn get_gem(&self, gem_id: U256) -> Result<Option<GemData>> {
        self.gem_items.get(gem_id)
    }

    pub fn token_of_owner_by_index(&self, owner: Address, index: u32) -> Result<U256> {
        let count = self.owner_gem_counts.read(&owner)?;
        if index >= count {
            return Err(GemError::IndexOutOfBounds.into());
        }
        self.owner_gem_ids
            .read(&Self::owner_index_key(owner, index))
    }

    pub fn token_uri(&self, gem_id: U256) -> Result<String> {
        let item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        let gem_id_hex = Self::format_gem_id(gem_id);
        let json = format!(
            "{{\"name\":\"Gem #{}\",\"description\":\"{}\",\"image\":\"{}{}\",\"attributes\":[{{\"trait_type\":\"gem_id\",\"value\":\"{}\"}},{{\"trait_type\":\"gem_type\",\"value\":{}}},{{\"trait_type\":\"state\",\"value\":{}}},{{\"trait_type\":\"gem_load\",\"value\":\"{}\"}},{{\"trait_type\":\"entry_price\",\"value\":\"{}\"}},{{\"trait_type\":\"cost_amount\",\"value\":\"{}\"}},{{\"trait_type\":\"floor_price\",\"value\":\"{}\"}},{{\"trait_type\":\"issuance_currency\",\"value\":{}}},{{\"trait_type\":\"reference_currency\",\"value\":{}}}]}}",
            &gem_id_hex[..8],
            TOKEN_DESCRIPTION,
            TOKEN_IMAGE_BASE,
            gem_id_hex,
            gem_id,
            item.gem_type,
            item.state,
            item.gem_load,
            item.entry_price,
            item.cost_amount,
            item.floor_price,
            item.issuance_currency,
            item.reference_currency,
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        Ok(format!("data:application/json;base64,{}", encoded))
    }

    pub(crate) fn owner_index_key(owner: Address, index: u32) -> B256 {
        let mut buf = [0u8; 24];
        buf[0..20].copy_from_slice(owner.as_slice());
        buf[20..24].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    pub(crate) fn add_gem(&mut self, item: &GemData) -> Result<()> {
        if self.gem_items.exists(item.gem_id)? {
            return Err(GemError::AlreadyExists.into());
        }
        self.gem_items.create(item)?;

        let owner_count = self.owner_gem_counts.read(&item.owner)?;
        self.owner_gem_ids
            .write(&Self::owner_index_key(item.owner, owner_count), item.gem_id)?;
        self.owner_gem_counts.write(&item.owner, owner_count + 1)?;

        let idx = self.all_gem_ids.len()?;
        self.all_gem_ids.push(item.gem_id)?;
        self.gem_index.write(&item.gem_id, idx)?;

        let supply = self.total_supply.read()?;
        self.total_supply.write(supply + 1)?;

        // Park unqualified gems in the bin index so the qualifier hook can
        // skip non-candidates without scanning the full population.
        if item.state == GemState::Issued as u8 {
            self.insert_unqualified(item.gem_id, item.floor_price)?;
        } else if item.state == GemState::Qualified as u8 {
            // Genesis gems are born Qualified — index them as callable gems.
            self.insert_callable(item.gem_id)?;
        }

        Ok(())
    }

    pub(crate) fn burn(&mut self, item: &GemData) -> Result<()> {
        self.gem_items.delete(item.gem_id)?;

        // Drop the callable-gem entry when burning a Qualified/Called gem
        // (forfeit). Settled gems (promis mining) were never listed.
        if item.state == GemState::Qualified as u8 || item.state == GemState::Called as u8 {
            self.remove_callable(item.gem_id)?;
        }

        let idx = self.gem_index.read(&item.gem_id)?;
        let last = self
            .all_gem_ids
            .len()?
            .checked_sub(1)
            .ok_or(GemError::GemNotFound)?;
        if idx != last {
            let last_id = self.all_gem_ids.get(last)?.ok_or(GemError::GemNotFound)?;
            self.all_gem_ids.set(idx, last_id)?;
            self.gem_index.write(&last_id, idx)?;
        }
        self.all_gem_ids.pop()?;
        self.gem_index.clear(&item.gem_id)?;

        self.compact_owner_index(item.owner, item.gem_id)?;

        let supply = self.total_supply.read()?;
        if supply > 0 {
            self.total_supply.write(supply - 1)?;
        }
        Ok(())
    }

    pub(crate) fn set_state(&mut self, gem_id: U256, new_state: GemState) -> Result<()> {
        let mut item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        // Issued is the only state parked in the bin index; any transition
        // out of Issued must clean it up. Idempotent if the gem isn't there.
        if item.state == GemState::Issued as u8 && new_state != GemState::Issued {
            self.remove_unqualified(gem_id, item.floor_price)?;
        }

        // Maintain the callable-gem list (membership == Qualified/Called).
        match new_state {
            // Issued -> Qualified enters the list.
            GemState::Qualified => self.insert_callable(gem_id)?,
            // Qualified|Called -> Settled leaves it. An Issued -> Settled jump
            // was never listed, so skip it.
            GemState::Settled if item.state != GemState::Issued as u8 => {
                self.remove_callable(gem_id)?
            }
            _ => {}
        }

        item.state = new_state as u8;
        self.gem_items.update(&item)?;
        Ok(())
    }

    /// Append a gem to the dense callable-gem list.
    fn insert_callable(&mut self, gem_id: U256) -> Result<()> {
        let idx = self.callable_gems.len()?;
        self.callable_gems.push(gem_id)?;
        self.callable_gem_index.write(&gem_id, idx)?;
        Ok(())
    }

    /// Swap-remove a gem from the callable-gem list. Caller guarantees the gem
    /// is currently listed (state was Qualified or Called).
    fn remove_callable(&mut self, gem_id: U256) -> Result<()> {
        let idx = self.callable_gem_index.read(&gem_id)?;
        let last = self
            .callable_gems
            .len()?
            .checked_sub(1)
            .ok_or(GemError::GemNotFound)?;
        if idx != last {
            let last_id = self.callable_gems.get(last)?.ok_or(GemError::GemNotFound)?;
            self.callable_gems.set(idx, last_id)?;
            self.callable_gem_index.write(&last_id, idx)?;
        }
        self.callable_gems.pop()?;
        self.callable_gem_index.clear(&gem_id)?;
        Ok(())
    }

    /// `Qualified -> Called`. Records the call timestamp used to enforce the
    /// notice-period settlement deadline. Qualified gems are not parked in the
    /// unqualified bin index, so there is nothing to clean up here.
    pub(crate) fn mark_called(&mut self, gem_id: U256, called_at: u32) -> Result<()> {
        let mut item = self.gem_items.get(gem_id)?.ok_or(GemError::GemNotFound)?;
        if item.state != GemState::Qualified as u8 {
            return Err(GemError::InvalidState.into());
        }
        item.state = GemState::Called as u8;
        item.called_at = called_at;
        self.gem_items.update(&item)?;
        Ok(())
    }

    fn compact_owner_index(&mut self, owner: Address, gem_id: U256) -> Result<()> {
        let count = self.owner_gem_counts.read(&owner)?;
        let last = count.checked_sub(1).ok_or(GemError::GemNotFound)?;
        let mut found: Option<u32> = None;
        for i in 0..count {
            let key = Self::owner_index_key(owner, i);
            if self.owner_gem_ids.read(&key)? == gem_id {
                found = Some(i);
                break;
            }
        }
        let idx = found.ok_or(GemError::GemNotFound)?;
        let last_key = Self::owner_index_key(owner, last);
        if idx != last {
            let last_id = self.owner_gem_ids.read(&last_key)?;
            self.owner_gem_ids
                .write(&Self::owner_index_key(owner, idx), last_id)?;
        }
        self.owner_gem_ids.clear(&last_key)?;
        self.owner_gem_counts.write(&owner, last)?;
        Ok(())
    }

    // --- Unqualified-gem bin index (PancakeSwap LB-style) ----------------

    pub fn price_to_bin(price: U256) -> Result<u32> {
        if price.is_zero() {
            return Ok(0);
        }
        let p_128x128 = price_helper::convert_decimal_price_to_128x128(price)?;
        price_helper::get_id_from_price(p_128x128, BIN_STEP_BP)
    }

    pub(crate) fn bin_index_key(bin_id: u32, index: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&bin_id.to_be_bytes());
        buf[4..8].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    pub(crate) fn insert_unqualified(&mut self, gem_id: U256, floor_price: U256) -> Result<()> {
        let bin_id = Self::price_to_bin(floor_price)?;
        debug_assert!(bin_id <= MAX_BIN_ID);
        let count = self.unqualified_bin_count.read(&bin_id)?;
        self.unqualified_bin_gems
            .write(&Self::bin_index_key(bin_id, count), gem_id)?;
        self.unqualified_bin_count.write(&bin_id, count + 1)?;
        tree_math::add(self, bin_id)?;
        Ok(())
    }

    /// Remove `gem_id` from the bin at its `floor_price`. Performs swap-and-pop
    /// to keep the bin's index dense; clears the bin's trie bit when emptied.
    pub(crate) fn remove_unqualified(&mut self, gem_id: U256, floor_price: U256) -> Result<()> {
        let bin_id = Self::price_to_bin(floor_price)?;
        let count = self.unqualified_bin_count.read(&bin_id)?;
        if count == 0 {
            return Ok(());
        }
        let mut found: Option<u32> = None;
        for i in 0..count {
            let key = Self::bin_index_key(bin_id, i);
            if self.unqualified_bin_gems.read(&key)? == gem_id {
                found = Some(i);
                break;
            }
        }
        let Some(idx) = found else {
            return Ok(());
        };
        let last = count - 1;
        let last_key = Self::bin_index_key(bin_id, last);
        if idx != last {
            let last_id = self.unqualified_bin_gems.read(&last_key)?;
            self.unqualified_bin_gems
                .write(&Self::bin_index_key(bin_id, idx), last_id)?;
        }
        self.unqualified_bin_gems.clear(&last_key)?;
        self.unqualified_bin_count.write(&bin_id, last)?;
        if last == 0 {
            tree_math::remove(self, bin_id)?;
        }
        Ok(())
    }
}

// Adapter between the contract's three bin-tree storage slots and the
// `tree_math::BinTreeStorage` trait.
impl BinTreeStorage for GemContract<'_> {
    fn read_root(&self) -> Result<U256> {
        self.bin_tree_root.read()
    }
    fn write_root(&self, value: U256) -> Result<()> {
        self.bin_tree_root.write(value)
    }
    fn read_mid(&self, key: u32) -> Result<U256> {
        self.bin_tree_mid.read(&key)
    }
    fn write_mid(&self, key: u32, value: U256) -> Result<()> {
        self.bin_tree_mid.write(&key, value)
    }
    fn read_leaf(&self, key: u32) -> Result<U256> {
        self.bin_tree_leaf.read(&key)
    }
    fn write_leaf(&self, key: u32, value: U256) -> Result<()> {
        self.bin_tree_leaf.write(&key, value)
    }
}
