//! A Solidity view reads the IntexFactory precompile as its daily VWAP source,
//! as the origin's IntexNFT1155 does.
use outbe_offchain_data::runtime_body_readers;
use std::sync::Arc;

use alloy_primitives::{Address, U256};
use alloy_sol_types::sol;
use outbe_evm::test_support::{install_code, runtime_code, seed_storage, EvmWorld};
use outbe_offchain_storage::MemoryStorage;
use outbe_oracle::test_support::record_finalized_day_vwap;
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use revm::database::{CacheDB, EmptyDB};

sol! {
    interface IVwapSourceReader {
        function read(address source, uint16 isoCode, uint32 fromUtcDay)
            external view returns (bool ok, uint256 vwap);
    }
}

const READER: Address = Address::new([0x77; 20]);
const CALLER: Address = Address::new([0x11; 20]);
const TIMESTAMP: u64 = 1_700_000_000;

#[test]
fn a_solidity_view_reads_the_factory_as_its_vwap_source() {
    let mut db = CacheDB::new(EmptyDB::default());
    install_code(
        &mut db,
        READER,
        &runtime_code(include_str!("fixtures/VwapSourceReader.hex")),
    );
    seed_storage(&mut db, TIMESTAMP, CALLER, |storage| {
        for (day, vwap) in [(20_231_110u32, 7u64), (20_231_111, 9)] {
            record_finalized_day_vwap(&storage, 840, day, U256::from(vwap)).unwrap();
        }
    });

    let mut world = EvmWorld::new(db)
        .at(TIMESTAMP)
        .with_readers(runtime_body_readers(Arc::new(MemoryStorage::new())));
    let mut read = |iso_code: u16, from_utc_day: u32| {
        let read = world.view(
            CALLER,
            READER,
            IVwapSourceReader::readCall {
                source: INTEX_FACTORY_ADDRESS,
                isoCode: iso_code,
                fromUtcDay: from_utc_day,
            },
        );
        (read.ok, read.vwap)
    };

    assert_eq!(read(840, 20_231_110), (true, U256::from(9)));
    assert_eq!(read(840, 20_231_112), (true, U256::ZERO));
    assert_eq!(read(978, 20_231_110), (true, U256::ZERO));
}
