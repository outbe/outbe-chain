//! Local storage helpers for the IntexFactory module (settlement bookkeeping
//! + the unqualified-series bin index). Orchestration lives in `runtime.rs`.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::error::Result;
use outbe_primitives::math::{
    price_helper,
    tree_math::{self, BinTreeStorage},
};
use outbe_primitives::storage::dsl::Map;

use crate::constants::BIN_STEP_BP;
use crate::schema::IntexFactoryContract;

impl IntexFactoryContract<'_> {
    // --- authorizedSettler ---

    pub(crate) fn read_authorized_settler(
        &self,
        holder: Address,
        series_id: u32,
    ) -> Result<Address> {
        let key = Self::authorized_settler_key(holder, series_id);
        self.authorized_settler.read(&key)
    }

    pub(crate) fn write_authorized_settler(
        &mut self,
        holder: Address,
        series_id: u32,
        settler: Address,
    ) -> Result<()> {
        let key = Self::authorized_settler_key(holder, series_id);
        self.authorized_settler.write(&key, settler)
    }

    // --- settleCount ---

    pub(crate) fn bump_settle_count(&mut self, series_id: u32) -> Result<()> {
        let current = self.settle_count.read(&series_id)?;
        self.settle_count
            .write(&series_id, current.saturating_add(U256::from(1)))
    }

    // --- mineSeq ---

    pub(crate) fn read_mine_seq(&self, series_id: u32, holder: Address) -> Result<u32> {
        let key = Self::mine_seq_key(series_id, holder);
        self.mine_seq.read(&key)
    }

    pub(crate) fn write_mine_seq(
        &mut self,
        series_id: u32,
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

    pub(crate) fn bin_index_key(bin_id: u32, index: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&bin_id.to_be_bytes());
        buf[4..8].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }

    pub(crate) fn insert_unqualified(&mut self, series_id: u32, floor_price: U256) -> Result<()> {
        insert_bin(
            &self.unqualified_bin_count,
            &self.unqualified_bin_series,
            &*self,
            series_id,
            floor_price,
        )
    }

    pub(crate) fn remove_unqualified(&mut self, series_id: u32, floor_price: U256) -> Result<()> {
        remove_bin(
            &self.unqualified_bin_count,
            &self.unqualified_bin_series,
            &*self,
            series_id,
            floor_price,
        )
    }

    // --- qualified-series bin index (by call_price_minor) ---

    pub(crate) fn insert_qualified(&mut self, series_id: u32, trigger_price: U256) -> Result<()> {
        insert_bin(
            &self.qualified_bin_count,
            &self.qualified_bin_series,
            &QualifiedBinTree(&*self),
            series_id,
            trigger_price,
        )
    }

    pub(crate) fn remove_qualified(&mut self, series_id: u32, trigger_price: U256) -> Result<()> {
        remove_bin(
            &self.qualified_bin_count,
            &self.qualified_bin_series,
            &QualifiedBinTree(&*self),
            series_id,
            trigger_price,
        )
    }
}

/// Insert `series_id` into the `price` bin of an index and set the trie bit.
fn insert_bin(
    count_map: &Map<u32, u32>,
    series_map: &Map<B256, u32>,
    tree: &impl BinTreeStorage,
    series_id: u32,
    price: U256,
) -> Result<()> {
    let bin_id = IntexFactoryContract::price_to_bin(price)?;
    let count = count_map.read(&bin_id)?;
    series_map.write(
        &IntexFactoryContract::bin_index_key(bin_id, count),
        series_id,
    )?;
    count_map.write(&bin_id, count + 1)?;
    tree_math::add(tree, bin_id)?;
    Ok(())
}

/// Remove `series_id` from its `price` bin (swap-and-pop); clear the trie bit
/// when the bin empties. No-op if absent.
fn remove_bin(
    count_map: &Map<u32, u32>,
    series_map: &Map<B256, u32>,
    tree: &impl BinTreeStorage,
    series_id: u32,
    price: U256,
) -> Result<()> {
    let bin_id = IntexFactoryContract::price_to_bin(price)?;
    let count = count_map.read(&bin_id)?;
    if count == 0 {
        return Ok(());
    }
    let mut found: Option<u32> = None;
    for i in 0..count {
        if series_map.read(&IntexFactoryContract::bin_index_key(bin_id, i))? == series_id {
            found = Some(i);
            break;
        }
    }
    let Some(idx) = found else {
        return Ok(());
    };
    let last = count - 1;
    let last_key = IntexFactoryContract::bin_index_key(bin_id, last);
    if idx != last {
        let last_id = series_map.read(&last_key)?;
        series_map.write(&IntexFactoryContract::bin_index_key(bin_id, idx), last_id)?;
    }
    series_map.clear(&last_key)?;
    count_map.write(&bin_id, last)?;
    if last == 0 {
        tree_math::remove(tree, bin_id)?;
    }
    Ok(())
}

/// Adapter between the contract's qualified bin-tree slots and `BinTreeStorage`,
/// so the call-trigger index reuses `tree_math` without a second trait impl on
/// the contract itself.
pub(crate) struct QualifiedBinTree<'a, 'b>(pub(crate) &'a IntexFactoryContract<'b>);

impl BinTreeStorage for QualifiedBinTree<'_, '_> {
    fn read_root(&self) -> Result<U256> {
        self.0.qualified_bin_tree_root.read()
    }
    fn write_root(&self, value: U256) -> Result<()> {
        self.0.qualified_bin_tree_root.write(value)
    }
    fn read_mid(&self, key: u32) -> Result<U256> {
        self.0.qualified_bin_tree_mid.read(&key)
    }
    fn write_mid(&self, key: u32, value: U256) -> Result<()> {
        self.0.qualified_bin_tree_mid.write(&key, value)
    }
    fn read_leaf(&self, key: u32) -> Result<U256> {
        self.0.qualified_bin_tree_leaf.read(&key)
    }
    fn write_leaf(&self, key: u32, value: U256) -> Result<()> {
        self.0.qualified_bin_tree_leaf.write(&key, value)
    }
}

// Adapter between the contract's three bin-tree slots and `BinTreeStorage`.
impl BinTreeStorage for IntexFactoryContract<'_> {
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
