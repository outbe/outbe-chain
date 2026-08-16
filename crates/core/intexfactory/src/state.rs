//! Local storage helpers for the IntexFactory module (settlement bookkeeping
//! + the unqualified-series bin index). Orchestration lives in `runtime.rs`.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_intex::SeriesId;
use outbe_primitives::error::Result;
use outbe_primitives::math::{
    price_helper,
    tree_math::{self, BinTreeStorage},
};
use outbe_primitives::storage::dsl::Map;
use outbe_primitives::storage::types::Storable;

use crate::constants::BIN_STEP_BP;
use crate::errors::IntexFactoryError;
use crate::schema::IntexFactoryContract;

impl IntexFactoryContract<'_> {
    // --- authorizedSettler ---

    pub(crate) fn read_authorized_settler(
        &self,
        holder: Address,
        series_id: SeriesId,
    ) -> Result<Address> {
        let key = Self::authorized_settler_key(holder, series_id);
        self.authorized_settler.read(&key)
    }

    pub(crate) fn write_authorized_settler(
        &mut self,
        holder: Address,
        series_id: SeriesId,
        settler: Address,
    ) -> Result<()> {
        let key = Self::authorized_settler_key(holder, series_id);
        self.authorized_settler.write(&key, settler)
    }

    // --- settleCount ---

    pub(crate) fn bump_settle_count(&mut self, series_id: SeriesId) -> Result<()> {
        let current = self.settle_count.read(&series_id)?;
        self.settle_count
            .write(&series_id, current.saturating_add(U256::from(1)))
    }

    // --- mineSeq ---

    pub(crate) fn read_mine_seq(&self, series_id: SeriesId, holder: Address) -> Result<u32> {
        let key = Self::mine_seq_key(series_id, holder);
        self.mine_seq.read(&key)
    }

    pub(crate) fn write_mine_seq(
        &mut self,
        series_id: SeriesId,
        holder: Address,
        value: u32,
    ) -> Result<()> {
        let key = Self::mine_seq_key(series_id, holder);
        self.mine_seq.write(&key, value)
    }

    // --- unqualified-series bin index (by floor_price_minor) ---

    /// Map an 18-decimal price to its LB-style bin id (bounded by the codec).
    pub fn price_to_bin(price: U256) -> Result<u32> {
        if price.is_zero() {
            return Ok(0);
        }
        let p = price_helper::convert_decimal_price_to_128x128(price)?;
        price_helper::get_id_from_price(p, BIN_STEP_BP)
    }

    pub(crate) fn bin_index_key(reference_currency: u16, bin_id: u32, index: u32) -> B256 {
        let mut buf = [0u8; 10];
        buf[0..2].copy_from_slice(&reference_currency.to_be_bytes());
        buf[2..6].copy_from_slice(&bin_id.to_be_bytes());
        buf[6..10].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    /// Composite key for a group's member list; the layout `bin_index_key` uses,
    /// over a separate column keyed by worldwide day instead of bin id.
    pub(crate) fn group_member_key(
        reference_currency: u16,
        worldwide_day: WorldwideDay,
        index: u32,
    ) -> B256 {
        Self::bin_index_key(reference_currency, worldwide_day.value(), index)
    }

    pub(crate) fn insert_unqualified(
        &mut self,
        series_id: SeriesId,
        reference_currency: u16,
        floor_price: U256,
    ) -> Result<()> {
        let bin_id = Self::price_to_bin(floor_price)?;
        self.unqualified_index(reference_currency).insert(
            &UnqualifiedBinTree(&*self, reference_currency),
            series_id,
            bin_id,
        )
    }

    pub(crate) fn remove_unqualified_group(
        &mut self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<()> {
        self.unqualified_index(reference_currency).remove_group(
            &UnqualifiedBinTree(&*self, reference_currency),
            worldwide_day,
        )
    }

    pub(crate) fn unqualified_groups_in_bin(
        &self,
        reference_currency: u16,
        bin_id: u32,
    ) -> Result<Vec<WorldwideDay>> {
        self.unqualified_index(reference_currency)
            .groups_in_bin(bin_id)
    }

    pub(crate) fn unqualified_group_members(
        &self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<Vec<SeriesId>> {
        self.unqualified_index(reference_currency)
            .members(worldwide_day)
    }

    pub(crate) fn unqualified_group(
        &self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<Group> {
        Ok(Group {
            iso_code: reference_currency,
            worldwide_day,
            members: self.unqualified_group_members(reference_currency, worldwide_day)?,
        })
    }

    // --- qualified-series bin index (by call_price_minor) ---

    pub(crate) fn insert_qualified_group(
        &mut self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
        trigger_price: U256,
        members: &[SeriesId],
    ) -> Result<()> {
        let bin_id = Self::price_to_bin(trigger_price)?;
        self.qualified_index(reference_currency).insert_group(
            &QualifiedBinTree(&*self, reference_currency),
            worldwide_day,
            bin_id,
            members,
        )
    }

    pub(crate) fn remove_qualified_group(
        &mut self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<()> {
        self.qualified_index(reference_currency)
            .remove_group(&QualifiedBinTree(&*self, reference_currency), worldwide_day)
    }

    pub(crate) fn qualified_groups_in_bin(
        &self,
        reference_currency: u16,
        bin_id: u32,
    ) -> Result<Vec<WorldwideDay>> {
        self.qualified_index(reference_currency)
            .groups_in_bin(bin_id)
    }

    pub(crate) fn qualified_group_members(
        &self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<Vec<SeriesId>> {
        self.qualified_index(reference_currency)
            .members(worldwide_day)
    }

    pub(crate) fn qualified_group(
        &self,
        reference_currency: u16,
        worldwide_day: WorldwideDay,
    ) -> Result<Group> {
        Ok(Group {
            iso_code: reference_currency,
            worldwide_day,
            members: self.qualified_group_members(reference_currency, worldwide_day)?,
        })
    }
}

impl<'storage> IntexFactoryContract<'storage> {
    fn unqualified_index(&self, reference_currency: u16) -> GroupIndex<'storage> {
        GroupIndex {
            bin_count: self.unqualified_bin_count.clone(),
            bin_groups: self.unqualified_bin_groups.clone(),
            group_count: self.unqualified_group_count.clone(),
            group_members: self.unqualified_group_members.clone(),
            group_bin: self.unqualified_group_bin.clone(),
            iso: reference_currency,
        }
    }

    fn qualified_index(&self, reference_currency: u16) -> GroupIndex<'storage> {
        GroupIndex {
            bin_count: self.qualified_bin_count.clone(),
            bin_groups: self.qualified_bin_groups.clone(),
            group_count: self.qualified_group_count.clone(),
            group_members: self.qualified_group_members.clone(),
            group_bin: self.qualified_group_bin.clone(),
            iso: reference_currency,
        }
    }
}

/// One day's series in one reference currency: the unit a lifecycle decision is
/// taken over, since all of them share the inputs it reads.
pub(crate) struct Group {
    pub(crate) iso_code: u16,
    pub(crate) worldwide_day: WorldwideDay,
    pub(crate) members: Vec<SeriesId>,
}

/// One currency's two-level index: price bins hold worldwide-day groups, and
/// each group holds the series that share its decision inputs.
struct GroupIndex<'storage> {
    bin_count: Map<'storage, u64, u32>,
    bin_groups: Map<'storage, B256, u32>,
    group_count: Map<'storage, u64, u32>,
    group_members: Map<'storage, B256, U256>,
    group_bin: Map<'storage, u64, u32>,
    iso: u16,
}

impl GroupIndex<'_> {
    fn group_key(&self, worldwide_day: WorldwideDay) -> u64 {
        IntexFactoryContract::scoped(self.iso, worldwide_day.value())
    }

    fn member_key(&self, worldwide_day: WorldwideDay, index: u32) -> B256 {
        IntexFactoryContract::group_member_key(self.iso, worldwide_day, index)
    }

    fn bin_key(&self, bin_id: u32, index: u32) -> B256 {
        IntexFactoryContract::bin_index_key(self.iso, bin_id, index)
    }

    fn groups_in_bin(&self, bin_id: u32) -> Result<Vec<WorldwideDay>> {
        let count = self
            .bin_count
            .read(&IntexFactoryContract::scoped(self.iso, bin_id))?;
        let mut groups = Vec::with_capacity(count as usize);
        for index in 0..count {
            groups.push(WorldwideDay::new(
                self.bin_groups.read(&self.bin_key(bin_id, index))?,
            ));
        }
        Ok(groups)
    }

    fn members(&self, worldwide_day: WorldwideDay) -> Result<Vec<SeriesId>> {
        let count = self.group_count.read(&self.group_key(worldwide_day))?;
        let mut members = Vec::with_capacity(count as usize);
        for index in 0..count {
            members.push(SeriesId::from_word(
                self.group_members
                    .read(&self.member_key(worldwide_day, index))?,
            ));
        }
        Ok(members)
    }

    /// Append `series_id` to its day's group, creating the group in `bin_id` when
    /// it is the first member. A later member priced into another bin would split
    /// the group's single decision in two, so it is refused loudly.
    fn insert(&self, tree: &impl BinTreeStorage, series_id: SeriesId, bin_id: u32) -> Result<()> {
        let worldwide_day = series_id.worldwide_day();
        let group_key = self.group_key(worldwide_day);
        let count = self.group_count.read(&group_key)?;
        if count == 0 {
            self.attach(tree, worldwide_day, bin_id)?;
        } else {
            let expected = self.group_bin.read(&group_key)?;
            if expected != bin_id {
                return Err(IntexFactoryError::GroupBinMismatch {
                    iso: self.iso,
                    worldwide_day,
                    expected,
                    got: bin_id,
                }
                .into());
            }
        }
        self.group_members
            .write(&self.member_key(worldwide_day, count), series_id.to_word())?;
        self.group_count.write(&group_key, count + 1)?;
        Ok(())
    }

    /// Index a whole group at once: its members and its place in `bin_id`.
    fn insert_group(
        &self,
        tree: &impl BinTreeStorage,
        worldwide_day: WorldwideDay,
        bin_id: u32,
        members: &[SeriesId],
    ) -> Result<()> {
        if members.is_empty() {
            return Ok(());
        }
        let group_key = self.group_key(worldwide_day);
        if self.group_count.read(&group_key)? != 0 {
            return Err(IntexFactoryError::GroupAlreadyIndexed {
                iso: self.iso,
                worldwide_day,
            }
            .into());
        }
        self.attach(tree, worldwide_day, bin_id)?;
        for (index, series_id) in members.iter().enumerate() {
            self.group_members.write(
                &self.member_key(worldwide_day, index as u32),
                series_id.to_word(),
            )?;
        }
        self.group_count.write(&group_key, members.len() as u32)?;
        Ok(())
    }

    /// Drop a whole group: its members and its place in the bin.
    fn remove_group(&self, tree: &impl BinTreeStorage, worldwide_day: WorldwideDay) -> Result<()> {
        let group_key = self.group_key(worldwide_day);
        let count = self.group_count.read(&group_key)?;
        if count == 0 {
            return Ok(());
        }
        for index in 0..count {
            self.group_members
                .clear(&self.member_key(worldwide_day, index))?;
        }
        self.group_count.write(&group_key, 0)?;
        let bin_id = self.group_bin.read(&group_key)?;
        self.detach(tree, worldwide_day, bin_id)
    }

    /// Register the group in `bin_id` and set the bin's trie bit.
    fn attach(
        &self,
        tree: &impl BinTreeStorage,
        worldwide_day: WorldwideDay,
        bin_id: u32,
    ) -> Result<()> {
        let scoped = IntexFactoryContract::scoped(self.iso, bin_id);
        let count = self.bin_count.read(&scoped)?;
        self.bin_groups
            .write(&self.bin_key(bin_id, count), worldwide_day.value())?;
        self.bin_count.write(&scoped, count + 1)?;
        self.group_bin
            .write(&self.group_key(worldwide_day), bin_id)?;
        tree_math::add(tree, bin_id)?;
        Ok(())
    }

    /// Drop the group's bin entry (swap-and-pop); clear the trie bit when the bin
    /// empties.
    fn detach(
        &self,
        tree: &impl BinTreeStorage,
        worldwide_day: WorldwideDay,
        bin_id: u32,
    ) -> Result<()> {
        let scoped = IntexFactoryContract::scoped(self.iso, bin_id);
        let count = self.bin_count.read(&scoped)?;
        if count == 0 {
            return Ok(());
        }
        let mut found: Option<u32> = None;
        for index in 0..count {
            if self.bin_groups.read(&self.bin_key(bin_id, index))? == worldwide_day.value() {
                found = Some(index);
                break;
            }
        }
        let Some(idx) = found else {
            return Ok(());
        };
        let last = count - 1;
        if idx != last {
            let last_day = self.bin_groups.read(&self.bin_key(bin_id, last))?;
            self.bin_groups
                .write(&self.bin_key(bin_id, idx), last_day)?;
        }
        self.bin_groups.clear(&self.bin_key(bin_id, last))?;
        self.bin_count.write(&scoped, last)?;
        self.group_bin.clear(&self.group_key(worldwide_day))?;
        if last == 0 {
            tree_math::remove(tree, bin_id)?;
        }
        Ok(())
    }
}

// Adapters between one currency's slice of a bin-tree's columns and `BinTreeStorage`.
// Construct inline at each `tree_math` call, so it never conflicts with a `&mut` borrow.

/// The unqualified (floor-price) trie of one reference currency.
pub(crate) struct UnqualifiedBinTree<'a, 'b>(
    pub(crate) &'a IntexFactoryContract<'b>,
    pub(crate) u16,
);

impl BinTreeStorage for UnqualifiedBinTree<'_, '_> {
    fn read_root(&self) -> Result<U256> {
        self.0.bin_tree_root.read(&self.1)
    }
    fn write_root(&self, value: U256) -> Result<()> {
        self.0.bin_tree_root.write(&self.1, value)
    }
    fn read_mid(&self, key: u32) -> Result<U256> {
        self.0
            .bin_tree_mid
            .read(&IntexFactoryContract::scoped(self.1, key))
    }
    fn write_mid(&self, key: u32, value: U256) -> Result<()> {
        self.0
            .bin_tree_mid
            .write(&IntexFactoryContract::scoped(self.1, key), value)
    }
    fn read_leaf(&self, key: u32) -> Result<U256> {
        self.0
            .bin_tree_leaf
            .read(&IntexFactoryContract::scoped(self.1, key))
    }
    fn write_leaf(&self, key: u32, value: U256) -> Result<()> {
        self.0
            .bin_tree_leaf
            .write(&IntexFactoryContract::scoped(self.1, key), value)
    }
}

/// The qualified (call-trigger) trie of one reference currency.
pub(crate) struct QualifiedBinTree<'a, 'b>(pub(crate) &'a IntexFactoryContract<'b>, pub(crate) u16);

impl BinTreeStorage for QualifiedBinTree<'_, '_> {
    fn read_root(&self) -> Result<U256> {
        self.0.qualified_bin_tree_root.read(&self.1)
    }
    fn write_root(&self, value: U256) -> Result<()> {
        self.0.qualified_bin_tree_root.write(&self.1, value)
    }
    fn read_mid(&self, key: u32) -> Result<U256> {
        self.0
            .qualified_bin_tree_mid
            .read(&IntexFactoryContract::scoped(self.1, key))
    }
    fn write_mid(&self, key: u32, value: U256) -> Result<()> {
        self.0
            .qualified_bin_tree_mid
            .write(&IntexFactoryContract::scoped(self.1, key), value)
    }
    fn read_leaf(&self, key: u32) -> Result<U256> {
        self.0
            .qualified_bin_tree_leaf
            .read(&IntexFactoryContract::scoped(self.1, key))
    }
    fn write_leaf(&self, key: u32, value: U256) -> Result<()> {
        self.0
            .qualified_bin_tree_leaf
            .write(&IntexFactoryContract::scoped(self.1, key), value)
    }
}
