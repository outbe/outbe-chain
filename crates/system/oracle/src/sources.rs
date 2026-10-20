//! Mainnet feeder sources compiled into the node, per protocol version.
//!
//! `sources/<major>.<minor>.toml` applies from that protocol version. The node
//! serves the file with the highest version that is not above the active one.
//! It also registers the file's pairs and stops voting on pairs that a newer
//! file drops. A released file never changes: a new
//! source set is a new file. `build.rs` reads the files at compile time.

use alloy_primitives::Address;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::asset_type::AssetType;
use outbe_primitives::chain::MAINNET_CHAIN_ID;
use outbe_primitives::error::Result;
use outbe_update::{encode_protocol_version, ProtocolVersion};

use crate::schema::OracleContract;

#[derive(Clone, Copy)]
enum Asset {
    Coen,
    Iso(u16),
}

impl Asset {
    fn address(self) -> Address {
        match self {
            Self::Coen => outbe_primitives::asset_type::COEN_ASSET,
            Self::Iso(code) => AssetType::IsoCurrency(code).into(),
        }
    }
}

struct Source {
    version: ProtocolVersion,
    toml: &'static str,
    pairs: &'static [(Asset, Asset)],
}

impl Source {
    fn pairs(&self) -> impl Iterator<Item = AddressPair> + '_ {
        self.pairs
            .iter()
            .map(|(base, quote)| AddressPair::from_addresses(base.address(), quote.address()))
    }
}

include!(concat!(env!("OUT_DIR"), "/sources.rs"));

/// The files in ascending version order. They are for mainnet only. Other
/// networks register pairs in genesis, and their feeders keep the pairs in the
/// local config.
fn table(chain_id: u64) -> &'static [Source] {
    if chain_id == MAINNET_CHAIN_ID {
        SOURCES
    } else {
        &[]
    }
}

fn select(table: &[Source], active: ProtocolVersion) -> Option<&Source> {
    table.iter().rev().find(|source| source.version <= active)
}

/// The feeder sources TOML for the network and the active protocol version,
/// or an empty string when the network has none.
pub fn sources_for(chain_id: u64, active: ProtocolVersion) -> &'static str {
    select(table(chain_id), active).map_or("", |source| source.toml)
}

/// Registers the pairs of the active file once per file. Pairs that the
/// previous file had and the active file drops stop being vote targets. They
/// keep their registry entry and price history.
pub(crate) fn sync_pairs(
    oracle: &mut OracleContract,
    chain_id: u64,
    active: ProtocolVersion,
) -> Result<()> {
    sync(oracle, table(chain_id), active)
}

fn sync(oracle: &mut OracleContract, table: &[Source], active: ProtocolVersion) -> Result<()> {
    let Some(current) = select(table, active) else {
        return Ok(());
    };
    // 0 means never synced, otherwise 1 + the raw version of the synced file.
    let marker = u64::from(current.version.raw()) + 1;
    let synced = oracle.sources_synced.read()?;
    if synced == marker {
        return Ok(());
    }
    let previous = table
        .iter()
        .find(|source| u64::from(source.version.raw()) + 1 == synced);
    for pair in previous.into_iter().flat_map(Source::pairs) {
        if !current.pairs().any(|kept| kept == pair) && oracle.pair_index_of(pair)? != 0 {
            oracle.vote_target.write(&pair, false)?;
        }
    }
    for pair in current.pairs() {
        if oracle.pair_index_of(pair)? == 0 {
            oracle.register_pair(pair)?;
        } else {
            oracle.vote_target.write(&pair, true)?;
        }
    }
    oracle.sources_synced.write(marker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    const EUR: (Asset, Asset) = (Asset::Coen, Asset::Iso(978));
    const USD: (Asset, Asset) = (Asset::Coen, Asset::Iso(840));
    const JPY: (Asset, Asset) = (Asset::Coen, Asset::Iso(392));

    fn pair((base, quote): (Asset, Asset)) -> AddressPair {
        AddressPair::from_addresses(base.address(), quote.address())
    }

    #[test]
    fn sync_registers_once_and_retires_dropped_pairs() {
        static TABLE: &[Source] = &[
            Source {
                version: encode_protocol_version(0, 0),
                toml: "",
                pairs: &[USD, EUR],
            },
            Source {
                version: encode_protocol_version(1, 4),
                toml: "",
                pairs: &[USD, JPY],
            },
        ];
        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let mut oracle = OracleContract::new(storage);
            sync(&mut oracle, TABLE, ProtocolVersion::ZERO).unwrap();
            sync(&mut oracle, TABLE, encode_protocol_version(1, 3)).unwrap();
            assert_eq!(oracle.pair_count.read().unwrap(), 2);
            assert!(oracle.vote_target.read(&pair(EUR)).unwrap());

            sync(&mut oracle, TABLE, encode_protocol_version(1, 4)).unwrap();
            assert_eq!(oracle.pair_count.read().unwrap(), 3);
            assert!(!oracle.vote_target.read(&pair(EUR)).unwrap());
            assert!(oracle.vote_target.read(&pair(USD)).unwrap());
            assert!(oracle.vote_target.read(&pair(JPY)).unwrap());
            assert_ne!(oracle.pair_index_of(pair(EUR)).unwrap(), 0);
        });
    }

    #[test]
    fn sync_keeps_a_pair_that_genesis_registered() {
        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let mut oracle = OracleContract::new(storage);
            oracle.register_pair(pair(USD)).unwrap();
            sync_pairs(&mut oracle, MAINNET_CHAIN_ID, ProtocolVersion::ZERO).unwrap();
            assert_eq!(oracle.pair_index_of(pair(USD)).unwrap(), 1);
            assert_eq!(
                oracle.pair_count.read().unwrap() as usize,
                SOURCES[0].pairs.len()
            );
        });
    }

    #[test]
    fn table_starts_at_zero_and_ascends() {
        assert_eq!(SOURCES[0].version, ProtocolVersion::ZERO);
        assert!(SOURCES
            .windows(2)
            .all(|pair| pair[0].version < pair[1].version));
    }

    #[test]
    fn a_network_without_files_serves_nothing() {
        assert_eq!(sources_for(424_242, ProtocolVersion::ZERO), "");
    }

    #[test]
    fn selects_the_newest_file_not_above_the_active_version() {
        static TABLE: &[Source] = &[
            Source {
                version: encode_protocol_version(0, 0),
                toml: "base",
                pairs: &[],
            },
            Source {
                version: encode_protocol_version(1, 4),
                toml: "v1.4",
                pairs: &[],
            },
        ];
        let toml = |active| select(TABLE, active).unwrap().toml;
        assert_eq!(toml(ProtocolVersion::ZERO), "base");
        assert_eq!(toml(encode_protocol_version(1, 3)), "base");
        assert_eq!(toml(encode_protocol_version(1, 4)), "v1.4");
        assert_eq!(toml(encode_protocol_version(2, 0)), "v1.4");
    }
}
