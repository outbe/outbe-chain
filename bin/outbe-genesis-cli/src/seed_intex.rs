//! Seed the target chains of IntexFactory.

use alloy_primitives::U256;
use serde_json::Value;

use crate::error::GenesisError;
use crate::seed_codec::{data_slot, parse_u32, StorageBuilder};

/// IntexFactory slot 1: the target chains, each `chain_id | mode << 32`.
pub const INTEX_FACTORY_TARGET_CHAINS_SLOT: u64 = 1;
const MAX_TARGET_CHAINS: usize = 16;

/// Seeds `intex.target_chains`: `{chain_id, mode}` entries, where `own` names this network and
/// the mode is `series` or `auctions`.
pub fn seed_intex_targets(
    storage: &mut StorageBuilder,
    config: &Value,
    own_chain_id: u64,
) -> Result<(), GenesisError> {
    let entries = config
        .get("target_chains")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if entries.len() > MAX_TARGET_CHAINS {
        return Err(GenesisError::validation(format!(
            "intex.target_chains: at most {MAX_TARGET_CHAINS} chains"
        )));
    }
    let mut seen: Vec<u32> = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let chain_id = match entry.get("chain_id") {
            Some(Value::String(own)) if own == "own" => u32::try_from(own_chain_id)
                .map_err(|_| GenesisError::validation("intex.target_chains: chain id over u32"))?,
            Some(value) => parse_u32(value)?,
            None => {
                return Err(GenesisError::validation(
                    "intex.target_chains: entry without chain_id",
                ))
            }
        };
        let mode: u64 = match entry.get("mode").and_then(Value::as_str) {
            Some("series") => 1,
            Some("auctions") => 2,
            other => {
                return Err(GenesisError::validation(format!(
                "intex.target_chains: chain {chain_id} has mode {other:?}, not series or auctions"
            )))
            }
        };
        if chain_id == 0 || seen.contains(&chain_id) {
            return Err(GenesisError::validation(format!(
                "intex.target_chains: invalid or duplicate chain id {chain_id}"
            )));
        }
        seen.push(chain_id);
        storage.set_raw_slot(
            data_slot(INTEX_FACTORY_TARGET_CHAINS_SLOT) + U256::from(index),
            U256::from(u64::from(chain_id) | (mode << 32)),
        );
    }
    if !seen.is_empty() {
        storage.set_slot(INTEX_FACTORY_TARGET_CHAINS_SLOT, seen.len());
    }
    Ok(())
}
