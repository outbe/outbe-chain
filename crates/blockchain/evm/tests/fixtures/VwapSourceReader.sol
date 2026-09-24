// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/// Reads a daily VWAP source from a view, as IntexNFT1155.uri does, for vwap_source_view.rs.
/// Compile with solc 0.8.30 --optimize --evm-version prague --bin-runtime --metadata-hash none.
contract VwapSourceReader {
    function read(address source, uint16 isoCode, uint32 fromUtcDay) external view returns (bool ok, uint256 vwap) {
        bytes memory ret;
        (ok, ret) = source.staticcall(
            abi.encodeWithSignature("maxUtcDayVwapSince(uint16,uint32)", isoCode, fromUtcDay)
        );
        if (ok && ret.length >= 32) vwap = abi.decode(ret, (uint256));
    }
}
