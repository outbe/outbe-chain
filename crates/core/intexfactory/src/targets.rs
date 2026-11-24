//! The registry of the target chains and the chains of each day.

use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::time::EmissionDate;

use crate::schema::IntexFactoryContract;

pub const MAX_TARGET_CHAINS: usize = 16;

/// What a target chain gets: Series gets the series and their call notices, Auctions also
/// the auctions of Desis.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetMode {
    Off = 0,
    Series = 1,
    Auctions = 2,
}

impl TargetMode {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Off),
            1 => Some(Self::Series),
            2 => Some(Self::Auctions),
            _ => None,
        }
    }
}

pub fn pack_target(chain_id: u32, mode: TargetMode) -> u64 {
    u64::from(chain_id) | (u64::from(mode as u8) << 32)
}

fn unpack_target(word: u64) -> (u32, TargetMode) {
    let mode = TargetMode::from_u8((word >> 32) as u8).unwrap_or(TargetMode::Off);
    (word as u32, mode)
}

/// The chains that are not Off, with their modes, in registry order.
pub fn target_chains(storage: &StorageHandle<'_>) -> Result<Vec<(u32, TargetMode)>> {
    let factory = IntexFactoryContract::new(storage.clone());
    Ok(factory
        .target_chains
        .read_all()?
        .into_iter()
        .map(unpack_target)
        .filter(|(_, mode)| *mode != TargetMode::Off)
        .collect())
}

/// The chains that get the series and their call notices.
pub fn series_target_chains(storage: &StorageHandle<'_>) -> Result<Vec<u32>> {
    Ok(target_chains(storage)?
        .into_iter()
        .map(|(chain_id, _)| chain_id)
        .collect())
}

/// Freezes the chains in Auctions as the chains of `day` and returns them.
pub fn freeze_day_targets(storage: &StorageHandle<'_>, day: EmissionDate) -> Result<Vec<u32>> {
    let chains: Vec<u32> = target_chains(storage)?
        .into_iter()
        .filter(|(_, mode)| *mode == TargetMode::Auctions)
        .map(|(chain_id, _)| chain_id)
        .collect();
    let list = IntexFactoryContract::new(storage.clone())
        .day_targets
        .list(&day);
    list.clear()?;
    for chain_id in &chains {
        list.push(*chain_id)?;
    }
    Ok(chains)
}

/// The chains `day` started on; empty for a day that never started.
pub fn day_targets(storage: &StorageHandle<'_>, day: EmissionDate) -> Result<Vec<u32>> {
    IntexFactoryContract::new(storage.clone())
        .day_targets
        .list(&day)
        .read_all()
}
