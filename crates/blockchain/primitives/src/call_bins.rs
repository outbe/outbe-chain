//! Keys of the call-price bin index the rights' call sweeps walk.
//!
//! Each reference currency holds its own trie of bins on the Liquidity Book
//! ladder, and each bin a dense list of the entries priced into it.

use alloy_primitives::{keccak256, B256, U256};

use crate::daily_sweep::currency_position;
use crate::error::{PrecompileError, Result};
use crate::expiry_queue::QueueEntry;
use crate::math::constants::MAX_BIN_ID;
use crate::math::reference_price;
use crate::math::tree_math::{self, BinTreeStorage};
use crate::storage::dsl::{Map, Value};
use crate::sweep_budget::SweepBudget;

/// Bin step of the call-price ladder, in basis points.
pub const BIN_STEP_BP: u16 = 25;

/// The bin a six-decimal COEN/ISO call price falls in. A zero price sits in bin 0.
pub fn price_to_bin(price: U256) -> Result<u32> {
    if price.is_zero() {
        return Ok(0);
    }
    reference_price::coen_iso_price_to_bin_id(price, BIN_STEP_BP)
}

/// The lower edge of `bin` as a six-decimal COEN/ISO price.
pub fn bin_to_price_floor(bin: u32) -> Result<U256> {
    reference_price::bin_id_to_coen_iso_price(bin, BIN_STEP_BP)
}

/// Namespaces a bin column key by its reference currency.
///
/// Mapping keys are left-padded to 32 bytes before hashing, so a wider integer
/// type alone namespaces nothing: the ISO has to occupy real high bits. Bin ids
/// are 24-bit and the trie's mid and leaf keys 16-bit, so the low 32 bits always
/// hold `key` unambiguously.
pub const fn scoped(reference_currency: u16, key: u32) -> u64 {
    ((reference_currency as u64) << 32) | key as u64
}

/// Key of the `index`-th entry in bin `bin` of `reference_currency`.
pub fn bin_index_key(reference_currency: u16, bin: u32, index: u32) -> B256 {
    let mut buf = [0u8; 10];
    buf[0..2].copy_from_slice(&reference_currency.to_be_bytes());
    buf[2..6].copy_from_slice(&bin.to_be_bytes());
    buf[6..10].copy_from_slice(&index.to_be_bytes());
    keccak256(buf)
}

/// An entry's place in the index: its bin and `index + 1`, so 0 means absent.
pub const fn pack_slot(bin: u32, index: u32) -> u64 {
    ((bin as u64) << 32) | (index as u64 + 1)
}

pub const fn unpack_slot(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, (packed as u32).wrapping_sub(1))
}

/// A walk's place in a currency: the bin and the entries of it still to visit.
/// 0 entries left walks the bin from the top.
pub const fn pack_cursor(bin: u32, remaining: u32) -> u64 {
    ((bin as u64) << 32) | remaining as u64
}

pub const fn unpack_cursor(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

/// One reference currency's slice of a contract's bin index.
pub trait CallBinStore<'storage>: BinTreeStorage {
    type Entry: QueueEntry;

    fn currency(&self) -> u16;
    /// [`scoped`]`(currency, bin)` -> entries in the bin.
    fn bin_count(&self) -> &Map<'storage, u64, u32>;
    /// [`bin_index_key`] -> entry.
    fn bin_at(&self) -> &Map<'storage, B256, Self::Entry>;
    /// Entry -> [`pack_slot`]. 0 means not indexed.
    fn entry_slot(&self) -> &Map<'storage, Self::Entry, u64>;
    /// Currency -> [`pack_cursor`] of the walk in flight.
    fn scan_cursor(&self) -> &Map<'storage, u16, u64>;
}

fn corrupt(message: &str) -> PrecompileError {
    PrecompileError::Revert(format!("call bin index: {message}"))
}

/// The bin `entry` is indexed in, or `None` when it is not indexed.
pub fn bin_of<'s, S: CallBinStore<'s>>(store: &S, entry: S::Entry) -> Result<Option<u32>> {
    let packed = store.entry_slot().read(&entry)?;
    Ok((packed != 0).then(|| unpack_slot(packed).0))
}

pub fn len<'s, S: CallBinStore<'s>>(store: &S, bin: u32) -> Result<u32> {
    store.bin_count().read(&scoped(store.currency(), bin))
}

pub fn entry_at<'s, S: CallBinStore<'s>>(store: &S, bin: u32, index: u32) -> Result<S::Entry> {
    store
        .bin_at()
        .read(&bin_index_key(store.currency(), bin, index))
}

/// Appends `entry` to `bin`, lighting the bin in the trie when it was empty.
pub fn insert<'s, S: CallBinStore<'s>>(store: &S, entry: S::Entry, bin: u32) -> Result<()> {
    if store.entry_slot().read(&entry)? != 0 {
        return Err(corrupt("entry is already indexed"));
    }
    let iso = store.currency();
    let count = len(store, bin)?;
    let next = count
        .checked_add(1)
        .ok_or_else(|| corrupt("bin entry count overflow"))?;
    store
        .bin_at()
        .write(&bin_index_key(iso, bin, count), entry)?;
    store.bin_count().write(&scoped(iso, bin), next)?;
    store.entry_slot().write(&entry, pack_slot(bin, count))?;
    if count == 0 {
        tree_math::add(store, bin)?;
    }
    Ok(())
}

/// Swap-removes `entry` from its bin. Returns false for an entry not indexed.
pub fn remove<'s, S: CallBinStore<'s>>(store: &S, entry: S::Entry) -> Result<bool> {
    let packed = store.entry_slot().read(&entry)?;
    if packed == 0 {
        return Ok(false);
    }
    let iso = store.currency();
    let (bin, index) = unpack_slot(packed);
    if entry_at(store, bin, index)? != entry {
        return Err(corrupt("slot does not hold its entry"));
    }
    let last = len(store, bin)?
        .checked_sub(1)
        .filter(|last| index <= *last)
        .ok_or_else(|| corrupt("slot past the end of its bin"))?;
    let last_key = bin_index_key(iso, bin, last);
    if index != last {
        let moved = store.bin_at().read(&last_key)?;
        store
            .bin_at()
            .write(&bin_index_key(iso, bin, index), moved)?;
        store.entry_slot().write(&moved, pack_slot(bin, index))?;
    }
    store.bin_at().clear(&last_key)?;
    store.entry_slot().clear(&entry)?;
    if last == 0 {
        store.bin_count().clear(&scoped(iso, bin))?;
        tree_math::remove(store, bin)?;
    } else {
        store.bin_count().write(&scoped(iso, bin), last)?;
    }
    Ok(true)
}

/// What visiting one entry did to the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visit {
    /// The walk moves to the next entry.
    Next,
    /// The walk ends here and resumes at this entry next block.
    Stop,
}

/// Walks the currency's bins up to `ceiling`, lowest first, each from the top, so a
/// visit that swap-removes its entry only moves one already visited. Every entry
/// costs one visit. Resumes where the last walk stopped. Returns whether it ended.
pub fn walk<'s, S, F>(
    store: &S,
    ceiling: u32,
    budget: &mut SweepBudget,
    mut visit: F,
) -> Result<bool>
where
    S: CallBinStore<'s>,
    F: FnMut(S::Entry, &mut SweepBudget) -> Result<Visit>,
{
    let iso = store.currency();
    let (mut from_bin, mut remaining) = unpack_cursor(store.scan_cursor().read(&iso)?);
    while let Some(bin) =
        tree_math::find_first_left_inclusive(store, from_bin)?.filter(|bin| *bin <= ceiling)
    {
        let count = len(store, bin)?;
        let left = match bin == from_bin && remaining != 0 {
            true => remaining.min(count),
            false => count,
        };
        if let Some(stopped) = walk_bin(store, bin, left, budget, &mut visit)? {
            store.scan_cursor().write(&iso, pack_cursor(bin, stopped))?;
            return Ok(false);
        }
        match bin.checked_add(1) {
            Some(next) if next <= MAX_BIN_ID => from_bin = next,
            _ => break,
        }
        remaining = 0;
    }
    store.scan_cursor().write(&iso, 0)?;
    Ok(true)
}

/// Visits `bin` from its `remaining`-th entry down. Returns the entries still to
/// visit when the walk stopped inside it.
fn walk_bin<'s, S, F>(
    store: &S,
    bin: u32,
    mut remaining: u32,
    budget: &mut SweepBudget,
    visit: &mut F,
) -> Result<Option<u32>>
where
    S: CallBinStore<'s>,
    F: FnMut(S::Entry, &mut SweepBudget) -> Result<Visit>,
{
    while remaining > 0 {
        if !budget.visit() {
            return Ok(Some(remaining));
        }
        remaining -= 1;
        if visit(entry_at(store, bin, remaining)?, budget)? == Visit::Stop {
            return Ok(Some(remaining + 1));
        }
    }
    Ok(None)
}

/// Walks `currencies` from the one the cursor names. A currency closed behind the
/// cursor is never walked again, so every sweep ends. Returns whether all ended.
pub fn walk_currencies<F>(
    currencies: &[u16],
    cursor: &Value<'_, u32>,
    budget: &mut SweepBudget,
    mut per_currency: F,
) -> Result<bool>
where
    F: FnMut(u16, &mut SweepBudget) -> Result<bool>,
{
    let start = currency_position(currencies, cursor.read()?);
    for &iso in currencies.iter().skip(start) {
        if budget.visits_left() == 0 || !per_currency(iso, budget)? {
            cursor.write(u32::from(iso))?;
            return Ok(false);
        }
    }
    Ok(true)
}

/// Implements [`BinTreeStorage`] and [`CallBinStore`] for `Adapter(&contract, currency)`.
#[macro_export]
macro_rules! impl_call_bins {
    ($adapter:ident<$entry:ty> {
        root: $root:ident,
        mid: $mid:ident,
        leaf: $leaf:ident,
        count: $count:ident,
        at: $at:ident,
        slot: $slot:ident,
        cursor: $cursor:ident $(,)?
    }) => {
        $crate::impl_bin_tree_storage!($adapter scoped by $crate::call_bins::scoped {
            root: $root,
            mid: $mid,
            leaf: $leaf,
        });

        impl<'s> $crate::call_bins::CallBinStore<'s> for $adapter<'_, 's> {
            type Entry = $entry;
            fn currency(&self) -> u16 {
                self.1
            }
            fn bin_count(&self) -> &$crate::storage::dsl::Map<'s, u64, u32> {
                &self.0.$count
            }
            fn bin_at(&self) -> &$crate::storage::dsl::Map<'s, ::alloy_primitives::B256, $entry> {
                &self.0.$at
            }
            fn entry_slot(&self) -> &$crate::storage::dsl::Map<'s, $entry, u64> {
                &self.0.$slot
            }
            fn scan_cursor(&self) -> &$crate::storage::dsl::Map<'s, u16, u64> {
                &self.0.$cursor
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_and_cursors_round_trip() {
        assert_eq!(pack_slot(7, 0), (7 << 32) | 1);
        assert_eq!(unpack_slot(pack_slot(7, 3)), (7, 3));
        assert_eq!(unpack_cursor(pack_cursor(9, 5)), (9, 5));
        assert_eq!(pack_cursor(0, 0), 0);
    }

    #[test]
    fn a_zero_price_sits_in_bin_zero_and_prices_ascend_by_bin() {
        assert_eq!(price_to_bin(U256::ZERO).unwrap(), 0);
        let low = price_to_bin(U256::from(1_000_000u64)).unwrap();
        let high = price_to_bin(U256::from(2_000_000u64)).unwrap();
        assert!(low < high);
        assert!(bin_to_price_floor(high).unwrap() <= U256::from(2_000_000u64));
    }

    #[test]
    fn keys_are_namespaced_by_currency() {
        assert_ne!(scoped(840, 1), scoped(978, 1));
        assert_ne!(bin_index_key(840, 1, 0), bin_index_key(978, 1, 0));
    }
}
