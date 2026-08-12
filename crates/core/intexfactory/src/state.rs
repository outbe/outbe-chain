//! Local storage helpers for the IntexFactory module (settlement bookkeeping
//! + the unqualified-series bin index). Orchestration lives in `runtime.rs`.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_intex::SeriesId;
use outbe_primitives::error::Result;
use outbe_primitives::math::{
    price_helper,
    tree_math::{self, BinTreeStorage},
};
use outbe_primitives::storage::dsl::Map;
use outbe_primitives::storage::types::Storable;

use crate::constants::BIN_STEP_BP;
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

    pub(crate) fn insert_unqualified(
        &mut self,
        series_id: SeriesId,
        reference_currency: u16,
        floor_price: U256,
    ) -> Result<()> {
        insert_bin(
            &self.unqualified_bin_count,
            &self.unqualified_bin_series,
            &UnqualifiedBinTree(&*self, reference_currency),
            series_id,
            reference_currency,
            floor_price,
        )
    }

    pub(crate) fn remove_unqualified(
        &mut self,
        series_id: SeriesId,
        reference_currency: u16,
        floor_price: U256,
    ) -> Result<()> {
        remove_bin(
            &self.unqualified_bin_count,
            &self.unqualified_bin_series,
            &UnqualifiedBinTree(&*self, reference_currency),
            series_id,
            reference_currency,
            floor_price,
        )
    }

    // --- qualified-series bin index (by call_price_minor) ---

    pub(crate) fn insert_qualified(
        &mut self,
        series_id: SeriesId,
        reference_currency: u16,
        trigger_price: U256,
    ) -> Result<()> {
        insert_bin(
            &self.qualified_bin_count,
            &self.qualified_bin_series,
            &QualifiedBinTree(&*self, reference_currency),
            series_id,
            reference_currency,
            trigger_price,
        )
    }

    pub(crate) fn remove_qualified(
        &mut self,
        series_id: SeriesId,
        reference_currency: u16,
        trigger_price: U256,
    ) -> Result<()> {
        remove_bin(
            &self.qualified_bin_count,
            &self.qualified_bin_series,
            &QualifiedBinTree(&*self, reference_currency),
            series_id,
            reference_currency,
            trigger_price,
        )
    }
}

/// Insert `series_id` into the `price` bin of its currency's index and set the trie bit.
fn insert_bin(
    count_map: &Map<u64, u32>,
    series_map: &Map<B256, U256>,
    tree: &impl BinTreeStorage,
    series_id: SeriesId,
    reference_currency: u16,
    price: U256,
) -> Result<()> {
    let bin_id = IntexFactoryContract::price_to_bin(price)?;
    let scoped = IntexFactoryContract::scoped(reference_currency, bin_id);
    let count = count_map.read(&scoped)?;
    series_map.write(
        &IntexFactoryContract::bin_index_key(reference_currency, bin_id, count),
        series_id.to_word(),
    )?;
    count_map.write(&scoped, count + 1)?;
    tree_math::add(tree, bin_id)?;
    Ok(())
}

/// Remove `series_id` from its `price` bin (swap-and-pop); clear the trie bit
/// when the bin empties. No-op if absent.
fn remove_bin(
    count_map: &Map<u64, u32>,
    series_map: &Map<B256, U256>,
    tree: &impl BinTreeStorage,
    series_id: SeriesId,
    reference_currency: u16,
    price: U256,
) -> Result<()> {
    let bin_id = IntexFactoryContract::price_to_bin(price)?;
    let scoped = IntexFactoryContract::scoped(reference_currency, bin_id);
    let count = count_map.read(&scoped)?;
    if count == 0 {
        return Ok(());
    }
    let key_at =
        |index: u32| IntexFactoryContract::bin_index_key(reference_currency, bin_id, index);
    let mut found: Option<u32> = None;
    for i in 0..count {
        if series_map.read(&key_at(i))? == series_id.to_word() {
            found = Some(i);
            break;
        }
    }
    let Some(idx) = found else {
        return Ok(());
    };
    let last = count - 1;
    let last_key = key_at(last);
    if idx != last {
        let last_id = series_map.read(&last_key)?;
        series_map.write(&key_at(idx), last_id)?;
    }
    series_map.clear(&last_key)?;
    count_map.write(&scoped, last)?;
    if last == 0 {
        tree_math::remove(tree, bin_id)?;
    }
    Ok(())
}

// Adapters between one currency's slice of a bin-tree's three columns and
// `BinTreeStorage`, so both indexes reuse `tree_math` and neither currency can
// see another's bins. Construct the view inline at each `tree_math` call rather
// than binding it, so it never conflicts with a `&mut IntexFactoryContract`.

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
