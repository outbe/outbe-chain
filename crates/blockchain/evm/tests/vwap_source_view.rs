//! A Solidity view reads the IntexFactory precompile as its daily VWAP source, as the origin's IntexNFT1155 does.
use std::sync::Arc;

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall};
use outbe_compressed_entities::ExecutionScope;
use outbe_evm::sub_call;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::MemoryStorage;
use outbe_oracle::{api::AddressPair, schema::OracleContract};
use outbe_primitives::{
    addresses::INTEX_FACTORY_ADDRESS,
    block::BlockContext,
    chain::CHAIN_ID,
    storage::{direct::DirectStorageProvider, StorageHandle, SubCallInput, SubCallStatus},
};
use revm::{
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Context,
};

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
    let code = Bytecode::new_raw(Bytes::from(
        alloy_primitives::hex::decode(include_str!("fixtures/VwapSourceReader.hex").trim())
            .unwrap(),
    ));
    db.insert_account_info(
        READER,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
    let block = BlockContext::new(1, TIMESTAMP, CHAIN_ID, CALLER, vec![CALLER]);
    let mut provider = DirectStorageProvider::new(&mut db, block);
    StorageHandle::enter(&mut provider, |storage| {
        let oracle = OracleContract::new(storage.clone());
        let index =
            outbe_oracle::api::register_pair(storage.clone(), AddressPair::new_coen_to(840))
                .unwrap();
        for (day, vwap) in [(20_231_110u32, 7u64), (20_231_111, 9)] {
            oracle
                .utc_day_vwap_value
                .get_nested(&day)
                .write(&index, U256::from(vwap))
                .unwrap();
        }
        oracle
            .utc_day_vwap_last_finalized
            .write(20_231_111)
            .unwrap();
    });
    provider.flush().unwrap();

    let mut ctx = Context::mainnet()
        .with_db(db)
        .modify_cfg_chained(|cfg| cfg.chain_id = CHAIN_ID)
        .modify_block_chained(|block| block.timestamp = U256::from(TIMESTAMP));
    let readers = RuntimeBodyReaders::new(Arc::new(MemoryStorage::new()));
    let scope = Arc::new(ExecutionScope::new());
    let mut read = |iso_code: u16, from_utc_day: u32| {
        let out = sub_call::run(
            &mut ctx,
            CALLER,
            false,
            SpecId::PRAGUE,
            Some(readers.clone()),
            scope.clone(),
            SubCallInput {
                target: READER,
                value: U256::ZERO,
                calldata: IVwapSourceReader::readCall {
                    source: INTEX_FACTORY_ADDRESS,
                    isoCode: iso_code,
                    fromUtcDay: from_utc_day,
                }
                .abi_encode()
                .into(),
                gas_limit: 5_000_000,
                is_static: true,
            },
        )
        .unwrap();
        assert!(
            matches!(out.status, SubCallStatus::Success),
            "{:?}",
            out.status
        );
        let read = IVwapSourceReader::readCall::abi_decode_returns(&out.returndata).unwrap();
        (read.ok, read.vwap)
    };

    assert_eq!(read(840, 20_231_110), (true, U256::from(9)));
    assert_eq!(read(840, 20_231_112), (true, U256::ZERO));
    assert_eq!(read(978, 20_231_110), (true, U256::ZERO));
}
